# 访问记录流程与进程归属

本文说明一次实验中，程序发出的文件请求怎样变成报告里的输入、元数据访问、错误和提示，以及哪些请求按设计看不到。对应验收：`bash scripts/test-attribution.sh`（结果在 `artifacts/attribution-acceptance.json/.txt`），全套回归：`bash scripts/check.sh`（汇总在 `artifacts/regression-summary.json/.md`）。调用关系图见 [architecture.md](architecture.md)。

## 1. 一个请求经过的环节

1. 实验程序使用普通文件接口（open、read、stat、listdir、readlink），不需要接入日志 SDK。
2. Linux VFS 先做路径解析和权限检查。TeamFS 以 `default_permissions` 挂载，权限由内核按 mode 判断；目录项与属性 TTL 均为 0，所以每次路径解析都会向 TeamFS 发 LOOKUP，权限检查前常会先发 GETATTR 刷新属性。
3. FUSE 内核模块把操作写成请求，经 `/dev/fuse` 交给守护进程。请求头带 uid、gid 和 pid，**其中 pid 是调用线程的 ID（TID）**，多线程程序的不同线程会带不同的值。
4. fuse-rs 0.3.1 的 `Session::run` 在单个线程中逐个取出请求，调用 `src/filesystem.rs` 中 `TeamFs` 的对应回调，所有回调串行执行。
5. `TeamFs::run` 取得 `Service` 锁并执行业务操作，然后依次：调用 `Service::observe_experiment`（实验采集）、记录耗时指标、写结构化日志窗口；使用 `--trace` 时再输出一行原始回调。
6. `observe_experiment` 只在有进行中的采集会话时工作：先用 `getpgid(pid)` 得到调用者的进程组，再由 `observe_in_group` 按第 2 节的规则归入某个会话。
7. 主命令结束后，runner 通过 `.teamfs/control` 发送 `experiment_finish`；`finish_experiment` 对每个访问过的节点分类，为输入文件计算完整 SHA256，生成报告的 `capture`。

采集会话独立于结构化日志：日志窗口只保留最近 2,048 条，文件日志会轮转，而会话计数在整个运行期间累加，不受窗口影响。

## 2. 归属规则

| 调用者 | 访问对象 | 结果 | 记入报告的位置 |
| --- | --- | --- | --- |
| 本实验进程组 | 本实验固定快照中的节点 | 成功 | `inputs` 或 `metadata_accesses`：按节点累计 `operations`、`read_calls`、`read_bytes`、`pids` |
| 本实验进程组 | 本实验固定快照中的节点 | 失败 | `errors`：路径、操作、errno、次数；不计入该节点的操作数和 pids |
| 本实验进程组 | 快照之外的挂载内文件：业务树、其他快照、回收站 | 成功的内容请求（可读打开、read、readlink） | `unpinned_mount_accesses` 次数，`unpinned_mount_files` 按位置和访问时路径列出 |
| 其他进程组 | 本实验固定快照中的节点 | 成功的内容请求 | `other_group_snapshot_accesses` 次数和 `other_group_pids`（最多 16 个进程号），从不计入输入 |
| pid 为 0 的内核内部请求，或 `getpgid` 时调用者已退出 | — | — | 不记录 |

- **线程**：报告中的 `pids` 是进程号。FUSE 给出的线程 ID 通过 `/proc/<tid>/status` 的 `Tgid` 换算，换算结果在会话内缓存。结构化日志（`events.json`）里的 `caller.pid` 仍保留 FUSE 给出的原值，即线程 ID。
- **子进程**：runner 在执行门子进程里调用 `setpgid(0, 0)` 建立新进程组；普通 fork/exec 得到的子进程、孙进程继承进程组，会被计入，`summary.processes` 列出全部参与的进程。
- **离开进程组**：调用 `setsid()` 或 `setpgid()` 的进程不再被归属。如果它在运行期间读取同一个固定快照，这些请求会出现在 `other_group_snapshot_accesses` 中，`experiment run` 结束时打印警告、`experiment verify` 打印注意事项，以此提示采集可能不完整。快照以外的读取则完全看不到。
- **进程组的结束**：主命令退出后，runner 对整个进程组发送 SIGKILL，然后才结束采集；采集结束后到达的请求不再归属。

## 3. 访问分类

每个被访问的节点都有一个 `access_class`：

| 类别 | 判定条件 | 所在列表 | 典型来源 |
| --- | --- | --- | --- |
| `content_read` | 普通文件，至少一次成功 read（返回 0 字节也算） | `inputs` | cat、sha256sum、Python 读取；空文件读取 |
| `opened_only` | 普通文件，成功打开但没有 read | `inputs` | `wc -c` 只用 fstat 取大小；共享 mmap 失败后的打开 |
| `attribute_only` | 只有 lookup / getattr | `metadata_accesses` | stat、存在性检查、路径经过的目录 |
| `directory_listing` | 目录有 opendir / readdir | `metadata_accesses` | listdir、glob、find、ls、Python 导入时扫描目录 |
| `link_read` | 软链接有 readlink | `metadata_accesses` | 通过软链接打开文件、`ls -l` |

`capture.summary` 给出各类别的节点数、失败请求总数 `error_requests` 和参与进程 `processes`。从未被访问的文件，例如目录里没有读取的备用数据，不出现在报告中；只被列目录的文件同样不会出现。

`read_bytes` 是文件系统返回给程序的字节数，不能证明算法用了每个字节。`opened_only` 只说明文件被打开过，不声称正文被使用。

## 4. 路径与内容版本

- `path_bytes` 是相对快照根目录的原始字节路径，中文、非 UTF-8 字节、换行、制表符、反斜杠都原样保存，可以无损还原。快照根目录本身的路径为空数组。
- `path_display` 是可读形式：反斜杠写成 `\\`，控制字符写成 `\n`、`\t` 等，非法 UTF-8 字节写成 `\xff`。不同的字节路径不会显示成相同的字符串。
- `sha256` 针对整个文件版本计算，即使程序只读了其中几个字节。计算在守护进程内完成，不计入实验的读取次数。
- 失败的 lookup 记录为“父目录路径/请求的名称”，例如 `data/missing.csv`。

## 5. 边界与错误

- **缺失输入**：在固定快照内查找不存在的文件，记录为 `errors` 中的 lookup + ENOENT。
- **内核直接决定的失败不会到达 FUSE**，报告也不会补造：把普通文件当目录使用（如 `data.txt/x` 的 ENOTDIR），以及 `default_permissions` 拒绝的写打开（普通用户得到 EACCES）。以 root 运行时，写打开会到达 TeamFS，以 EROFS 记录在 `errors` 中。
- **未固定的挂载内输入**：绝对路径指向业务树、用 `../` 从快照根走到其他快照或业务树、读取回收站文件，都会列入 `unpinned_mount_files`，`all_observed_content_inputs_pinned` 为 false；`experiment verify/replay` 拒绝使用该报告，并在错误信息中列出路径。
- **越界软链接**：软链接目标按固定快照树解析，包括中间链接。绝对路径、越过快照根的 `..`、超过 40 层的链接链，都标记 `link_may_escape_input: true`；读取这类链接的次数计入 `external_link_accesses`，verify/replay 拒绝。树内相对链接链正常认证，悬空链接的目标查找记为 ENOENT。
- **mmap**：快照文件使用 direct I/O。共享映射（`MAP_SHARED`）返回 ENODEV，打开仍记录为 `opened_only`。私有只读映射可以使用，正文经内核页缓存读入，`read_calls/read_bytes` 是预读粒度：在本机 WSL2 内核上，触碰 100 字节时记录了 1 次、100,000 字节的读取。若同一文件页已被其他映射缓存，可能不再产生 FUSE 读取，因此 mmap 读取可能被低估。
- **未进入采集的请求**：getxattr / listxattr（未实现，fuse-rs 直接回复 ENOSYS）、statfs、flush、release、forget，以及快照中不可能出现的写操作。
- **进程号复用**：会话按进程组号匹配。runner 被 SIGKILL 后、执行 `experiment recover` 之前，会话仍处于采集中；理论上新进程组复用同一编号会被误认，实际需要进程号空间回绕，未做额外防护。
- **采集只覆盖挂载内文件**：外部脚本和库、其他磁盘文件、网络、环境变量、随机性均不在范围内。

## 6. 真实请求与报告对照

`tests/attribution.py` 以 `--trace` 启动守护进程。每个回调输出一行：

```text
[0123] read       ino=1152921504606847066 pid=29 {"fh":7,"offset":0,"requested_bytes":64} => OK bytes=64
```

测试把实验运行期间的回调逐行还原为“节点 → 操作次数、读取次数、返回字节、线程”，lookup 按“父目录 + 名称”还原到子节点；再与报告逐项比较：每个节点的 `operations`、`read_calls`、`read_bytes`、换算后的 `pids`、`errors`，以及其他进程组的内容请求数，全部必须相等。程序自己记录的读取次数、字节数和进程号也与报告比较。

| 用例 | 证明的内容 |
| --- | --- |
| direct / cached 两种挂载模式的对照运行 | 运行期间的全部回调与报告完全对上；五种分类；空文件；局部读取仍记录整文件 SHA256；线程归属到进程；子进程和孙进程计入；setsid 子进程和测试进程的读取只计入 `other_group_*` |
| 超过日志窗口的 2,700 次读取 | 读取大小 1～61 字节变化，日志窗口淘汰了旧记录，报告次数和字节仍与程序一致；运行中修改同名业务文件不影响固定版本 |
| 中文、非 UTF-8、控制字符名称 | `path_bytes` 无损，`path_display` 无歧义，二进制局部读取字节准确 |
| 缺失输入与内核侧失败 | ENOENT 记录；ENOTDIR 和 EACCES 不被补造 |
| 未固定输入 | 业务树、其他快照、回收站读取按位置和路径列出，verify 拒绝并给出路径 |
| 软链接 | 树内链接链认证；绝对、指向挂载业务树、`../` 越界链接被标记；悬空目标记 errno |
| mmap | 私有映射记录预读粒度的读取；共享映射单列为 unsupported |
| 实验模式下的常用工具 | ls、find、stat 只产生元数据；head 局部读取；wc -c 为 opened_only；sha256sum 计算的摘要与报告一致；tar、rsync 复制结果一致；git 读取仓库文件并记录探测的可选路径；vim 读取固定输入、写出到挂载点外 |

证据在 `artifacts/attribution-acceptance.json` 的 `context`：`request_report_crosscheck`（逐节点对照表、程序自记录、错误和其他进程组请求）、`beyond_audit_window`、`tools_in_experiments`。完整报告样例见 `artifacts/attribution-manifest-example.json`。

## 7. 报告字段约定（供比较功能和页面使用）

报告的 `schema_version` 仍为 1，本轮只增加字段，不改变已有字段的含义。读取旧报告时，新字段缺失应视为“未知”，不能当作 0。

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `capture.inputs[]`、`capture.metadata_accesses[]` | 数组 | 每个访问过的节点一行，按 `path_bytes` 排序 |
| ↳ `path_bytes` / `path_display` | 整数数组 / 字符串 | 见第 4 节 |
| ↳ `access_class` | 字符串 | 见第 3 节；本轮起元数据行也带此字段 |
| ↳ `operations` | 对象 | 成功操作次数，键为 lookup、getattr、open、read、readlink、opendir、readdir |
| ↳ `pids` | 整数数组 | 访问过该节点的进程号（不是线程号） |
| ↳ `read_calls` / `read_bytes` | 整数 | 成功 read 次数与返回字节 |
| ↳ `sha256` | 字符串 | 仅 `inputs`；完整文件版本 |
| ↳ `attributes` | 对象 | 快照中的 inode 属性 |
| ↳ `link_target_bytes` / `link_target_display` / `link_may_escape_input` | — | 仅软链接 |
| `capture.errors[]` | 数组 | `path_bytes`、`path_display`、`operation`、`errno`、`count` |
| `capture.summary` | 对象 | 五种类别的节点数、`error_requests`、`processes` |
| `capture.unpinned_mount_accesses` | 整数 | 未固定内容请求总数 |
| `capture.unpinned_mount_files[]` | 数组 | `location`（business / snapshot / trash）、`path_bytes`、`path_display`（访问时路径；已不在原路径时为 null）、`operations`、`read_bytes` |
| `capture.external_link_accesses` | 整数 | 读取越界软链接的次数 |
| `capture.other_group_snapshot_accesses` / `other_group_pids` | 整数 / 整数数组 | 其他进程组对固定快照的内容请求次数，最多保留 16 个进程号 |
| `capture.all_observed_content_inputs_pinned` | 布尔 | 无未固定读取且无越界链接时为 true |

比较两份报告时，应以 `path_bytes`、`sha256` 和 `access_class` 判断输入变化。`pids`、`process_group`、时间戳会随运行变化；`operations` 中 lookup / getattr 的次数也会随解释器的导入缓存等因素变化。这些都不表示文件内容变化。

`experiment run` 结束时把未固定读取、越界链接和其他进程组请求打印为“警告”。`experiment verify` 遇到前两种拒绝验证；只有第三种时验证照常进行，并打印“注意”。

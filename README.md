# TeamFS 0.2：Rust + FUSE 可恢复资料文件系统

TeamFS 使用原版 `fuse = 0.3.1`，在 Linux 用户空间处理普通文件操作。当前版本实现 **持久化保存、手动快照与文件恢复、虚拟状态文件**，并保留 `--memory` 教学模式。

## 一、先体验完整案例

在 Windows PowerShell 执行：

```powershell
wsl -d Ubuntu-24.04 --cd "/mnt/d/PyProject/operating system/Teamwork" -- bash scripts/showcase.sh
```

它在自己的临时挂载点和临时存储中演示：创建报告、同步、重挂载、创建快照、修改报告、恢复为新文件、读取状态、再次重挂载验证。结束时清理演示挂载和临时数据。

- `artifacts/showcase-output.txt`：真实命令输出。
- `artifacts/showcase-trace.log`：真实 FUSE 回调。
- `artifacts/showcase-metrics.json`：单次样例的快照命令耗时、逻辑内容大小、SQLite 实际文件占用及恢复内容 SHA256；不能据此推断普遍性能。
- `demo/index.html`：离线交互讲解，包含保存与恢复实验和原版内存请求流程。页面是模拟，不连接真实挂载。

原来的内存重置演示仍可运行：`bash scripts/showcase-memory.sh`。它保存独立的 `memory-showcase-*` 记录。

## 二、保存规则

| 行为 | persistent（默认） | memory |
| --- | --- | --- |
| 普通写入、改名、删除、恢复 | 先改内存，标记 dirty | 改内存 |
| 普通文件或目录 fsync、`teamfs sync` | 事务保存整个当前文件树 | 确认当前内存状态，没有磁盘保障 |
| 创建 / 删除快照 | 同时保存当前树和快照集合，成功后可跨重启保留 | 快照仅保留在当前进程 |
| 正常卸载 | 显式保存，失败时进程返回非零 | 进程结束，数据释放 |
| SIGKILL 等异常结束 | 恢复最后一次成功提交，未同步修改丢失 | 全部丢失 |

**关闭文件不是持久化保证。** `flush` 检查句柄，`fsync` 才同步；编辑器是否自动调用 fsync 取决于编辑器。需要明确保存时运行 `teamfs sync`。这里的同步命令指本程序的管理命令，不承诺系统全局 `sync` 在 direct_io 模式下具有相同效果。

普通文件读取会更新 atime，因此也可能使 dirty 变成 true；状态文件和控制文件访问不会计入业务读写或修改业务树。dirty 表示内存元数据/内容可能尚未提交，不仅表示有内容编辑。

SQLite 使用 WAL、`synchronous=FULL` 和事务保存。同步失败不会清空当前内存修改，也不会替换上一次完整提交；查看状态中的 `last_sync_error` 后修复存储问题，再重新同步。仅保证成功提交的状态，不承诺磁盘损坏、断电硬件故障或多个应用操作组成一个整体事务。

## 三、环境和启动

已使用 Ubuntu 24.04 WSL2、Rust/Cargo 1.75、libfuse 2.9.9 运行。Windows 原生 Rust 无法直接编译此 Linux 项目。新电脑在支持 FUSE 的 Linux 中先运行：

```bash
bash scripts/setup.sh
```

`rusqlite 0.31` 使用 bundled SQLite，不依赖系统 SQLite 开发包。`Cargo.lock` 固定实际依赖。

进入 Ubuntu，在项目目录启动（终端 A）：

```bash
cd "/mnt/d/PyProject/operating system/Teamwork"
bash scripts/start.sh
```

默认挂载点为 `$HOME/teamfs-mount`，默认数据库位于 `$HOME/.local/share/teamfs/default/state.sqlite3`。源码保留在 Teamwork，编译缓存默认在 `$HOME/.cache/teamfs/target`。

自定义挂载点与存储：

```bash
bash scripts/start.sh "$HOME/teamfs-work" --store "$HOME/.local/share/teamfs/work"
```

独立内存模式：

```bash
bash scripts/start.sh "$HOME/teamfs-memory" --memory
```

存储目录必须位于挂载点之外，包含符号链接和 `..` 的路径也会解析后检查。同一个存储目录只允许一个 TeamFS 进程使用。请将实际数据库放在 WSL Linux 文件系统中。

首次创建数据库时生成 `welcome.txt` 和 `notes/example.txt`。以后启动只加载保存的数据。数据库损坏、不支持的格式版本或非法目录关系会明确报错，不会自动重建覆盖。

## 四、管理命令与手动演示

终端 B 进入 Ubuntu，设置两个便捷变量（如覆盖了 CARGO_TARGET_DIR，请相应修改二进制路径）：

```bash
TEAMFS="$HOME/.cache/teamfs/target/debug/teamfs"
MOUNT="$HOME/teamfs-mount"
printf '报告初稿\n' > "$MOUNT/notes/report.txt"
"$TEAMFS" sync "$MOUNT"
"$TEAMFS" snapshot create "$MOUNT" before-edit
"$TEAMFS" snapshot list "$MOUNT"
printf '改错了\n' > "$MOUNT/notes/report.txt"
cat "$MOUNT/.teamfs/snapshots/before-edit/notes/report.txt"
"$TEAMFS" restore "$MOUNT" before-edit notes/report.txt notes/report-recovered.txt
cmp "$MOUNT/.teamfs/snapshots/before-edit/notes/report.txt" "$MOUNT/notes/report-recovered.txt"
"$TEAMFS" sync "$MOUNT"
cat "$MOUNT/.teamfs/status.json"
```

完整 CLI：

```text
teamfs mount <挂载点> [--store <目录> | --memory] [--trace]
teamfs sync <挂载点>
teamfs snapshot create <挂载点> <名称>
teamfs snapshot list <挂载点>
teamfs snapshot delete <挂载点> <名称>
teamfs restore <挂载点> <快照名称> <源相对路径> <目标相对路径>
```

恢复只接受普通文件；源和目标都相对于文件系统根目录。目标父目录必须存在，目标必须尚不存在。禁止绝对路径、`.`、`..`、空路径分量及 `.teamfs` 管理区。复制文件内容、权限和 mtime，分配新 inode；恢复后仍需同步或正常卸载。

快照名称为一个合法 UTF-8 文件名，支持中文；重复名称报错。最多 10 个快照，快照内容合计最多 128 MiB。超限拒绝，不自动清理历史。需要删除时显式运行 `snapshot delete`，已打开的历史文件仍可读到关闭。

普通 `cat`、`diff`、`cp` 可以访问快照目录。普通 `cp` 是否覆盖目标取决于 cp 参数；**默认不覆盖是 `teamfs restore` 的保证**。快照是本机历史保存点，不是异地备份。

正常卸载（在项目目录）：

```bash
bash scripts/unmount.sh "$MOUNT"
```

该脚本先显式同步；保存失败会退出并保留挂载。直接 `fusermount3 -u` 时，守护进程也会在挂载循环结束后保存，但调用者应查看挂载终端的退出结果。异常退出后的失效挂载可用 `fusermount3 -u <挂载点>` 清理，再以同一存储重启。

## 五、虚拟文件与状态字段

```text
挂载点/
├── welcome.txt
├── notes/
└── .teamfs/
    ├── status.json
    ├── control
    └── snapshots/
        └── before-edit/
            ├── welcome.txt
            └── notes/
```

`.teamfs` 是保留目录。历史树不包含管理目录，不能形成递归快照。快照文件与当前文件的 inode 不相同，历史数据只读。

`status.json` 每次打开生成一份固定 JSON，在同一句柄中分段读取始终一致；重新打开刷新。字段含义：

| 字段 | 含义 |
| --- | --- |
| schema_version / filesystem / version / mode | 状态格式版本、系统名、程序版本和模式 |
| uptime_seconds | 当前挂载运行时长 |
| files / directories | 当前有路径的业务文件和目录数，目录包括根目录，排除管理树 |
| used_bytes / capacity_bytes / max_file_bytes | 当前内容占用（含仍打开的已删除文件）、64 MiB 总上限、16 MiB 单文件上限 |
| snapshot_count / snapshot_bytes | 活跃快照数量和逻辑文件内容字节总量，重复内容按份计数 |
| snapshots | 名称和创建时间列表 |
| read_calls / write_calls / read_bytes / write_bytes | 本次挂载成功的业务回调次数和实际字节数，含历史文件读取，排除状态与控制请求；不是 shell 命令数 |
| dirty / last_sync_unix / last_sync_error | 未同步标记、最近成功同步 Unix 秒时间戳、最近同步错误 |

目录对象、索引、SQLite 页、WAL 以及句柄占用不包含在逻辑容量中，实际磁盘/内存占用会更大。删除快照后 SQLite 可以复用空闲页，数据库文件不保证立即缩小。

`.teamfs/control` 为管理客户端使用的只写文件：一个句柄缓冲一个最多 64 KiB 的 JSON 请求，显式 fsync 时执行一次。重复 fsync 返回同一结果，关闭不执行；格式无效不改变状态。命令通过挂载点进入同一个服务层，不直接写数据库。JSON 路径使用字节数组，支持非 UTF-8 文件名。

## 六、代码结构与实现决策

| 模块 | 职责 |
| --- | --- |
| src/main.rs | CLI、挂载检查、控制请求、正常卸载后的显式同步 |
| src/filesystem.rs | fuse-rs 回调适配、日志、reply |
| src/model.rs | inode、目录项、文件字节、句柄、引用回收、树导入导出 |
| src/service.rs | 保存边界、快照、虚拟 inode、状态文件和控制句柄 |
| src/store.rs | SQLite 格式校验、进程锁、事务读取与提交 |

业务树导出只保留有路径的节点，句柄、lookup 引用、运行计数器和已 unlink 的无路径节点不会恢复到下次挂载。数据库保存版本、当前树、快照树、原始文件名字节和属性。启动会检查父子关系、重复名称、容量和链接数。

快照固定在创建请求执行时的状态；不会把多次 write 或整个编辑器保存过程自动识别成应用事务。先完成编辑，再主动创建快照。

当前实现每次同步事务写入完整当前树和活跃快照，快照采用完整拷贝，没有去重/增量存储，适合当前小容量教学案例。服务沿用 fuse-rs 的串行请求处理，不进行后台异步保存。属性 TTL 为 0，文件使用 direct_io，便于观察回调，不以吞吐量最优为目标。

## 七、验收与边界

```bash
export CARGO_TARGET_DIR="$HOME/.cache/teamfs/target"
cargo test --locked
bash scripts/smoke.sh
bash scripts/test-v2.sh
bash scripts/showcase.sh
```

`smoke.sh` 使用内存模式复验原有 12 个挂载场景和 1 个重挂载场景。`test-v2.sh` 使用独立存储与挂载点，验证 15 组新增场景，包括 SIGKILL、只读快照、历史句柄、恢复、状态分段读取、控制请求幂等、SQLite 事务失败注入、重试、容量限制和损坏数据库拒绝。

现有 7 个模型测试之外，新增树导入校验、原始名称持久化、无路径节点排除、历史节点回收和控制请求边界测试，共 11 个测试。实际结果见 `artifacts/model-tests.txt`、`artifacts/acceptance.txt`、`artifacts/v2-acceptance.txt`。每次测试重写对应记录。

所有测试和完整演示都使用自己创建的挂载点，不会操作既有 `teamfs-playground`。旧进程不会因源码更新自动升级。

当前仍为单用户教学实现，使用 default_permissions，未开启 allow_other。没有实现符号链接、硬链接、完整 POSIX 兼容、自动历史、回收站、多用户协作、网络存储或硬件故障恢复。初期异常中断导致未完成初始化的数据库会被拒绝，需明确选择新的空存储目录，不能把它当成已有可靠存储使用。

## 八、答辩叙事与资料

项目基于 fuse-rs 开发应用，扩展的是 TeamFS 的存储与恢复能力，没有修改 fuse-rs 库。建议围绕同一份报告解释：普通读写如何到达 Rust、保存点怎样形成、快照如何隔离、错误发生后怎样取回内容。

展示实测记录时说明环境、样例大小及模式。11 个单元测试和真实挂载场景用于证明行为；单次 showcase 的耗时只代表该样例，不能宣称普遍性能优势。PPT 尚未制作，输出和测量记录可作为后续素材。

参考：[fuse-rs 官方仓库](https://github.com/zargony/fuse-rs)、[固定版本 API](https://docs.rs/fuse/0.3.1/fuse/)、[Linux FUSE](https://docs.kernel.org/filesystems/fuse/fuse.html)、[Rust 官方教程](https://doc.rust-lang.org/book/)、[任务参考文章](https://blog.csdn.net/gitblog_00096/article/details/138896027)。

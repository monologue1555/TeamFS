# TeamFS 0.6：Rust + FUSE 可恢复资料文件系统

TeamFS 使用原版 `fuse = 0.3.1`，在 Linux 用户空间处理普通文件操作。当前版本实现 **持久化保存、手动快照与文件恢复、虚拟状态文件、回收站、可选自动同步、快照差异清单**，新增 **整目录恢复与预览、独立备份与导入、软链接和实际软件兼容性、增量保存与按需读取**，新增 **结构化操作日志与只读真实监控**，新增 **兼容性自动验收、可复现随机/故障测试、离线自检与环境诊断、请求耗时分析和性能基线**，保留 `--memory` 教学模式。


## 0.6：正确性验收、诊断与性能分析

```bash
TEAMFS="$HOME/.cache/teamfs/target/debug/teamfs"
# 环境与挂载表诊断；不会隐式创建存储或卸载文件系统
"$TEAMFS" doctor "$HOME/teamfs-mount" --store "$HOME/.local/share/teamfs/default" --json
# 正常卸载后检查已有存储；正在挂载或被其他进程持锁时拒绝
"$TEAMFS" check "$HOME/.local/share/teamfs/default" --json
# 挂载期间查询耗时
"$TEAMFS" metrics "$HOME/teamfs-mount"
cat "$HOME/teamfs-mount/.teamfs/metrics.json"

bash scripts/test-v6.sh
bash scripts/benchmark-v6.sh --samples 3
# 固定种子重现一组随机实验
python3 tests/randomized.py --seed 1555 --steps 180 --mode direct
```

`check` 在现有 store.lock 上取得独占进程锁，以只读连接执行 SQLite integrity_check，复用格式、业务树、快照、回收记录与内容引用校验，额外拒绝不属于任何文件树的节点。它输出逻辑内容、blobs、无引用内容、数据库页和空闲页统计，不迁移、不清理、不修复。格式 1/2 可检查但不会升级；未知格式和损坏数据返回非零，JSON 仍包含错误与处理建议。无引用 blobs 是空间提示，不等同于损坏。检查需要已有锁文件；尚未首次挂载的导入副本可先使用 backup verify 检查源备份。

只读检查不修改应用数据；SQLite 打开带 WAL 的存储时可能管理 WAL/SHM 辅助文件。结构检查不能证明文件正文仍与最初原稿相同，因为当前没有持久保存每个内容版本的校验基准。备份完整性继续使用 backup verify 的 SHA256。

`doctor` 检查当前用户能否打开 /dev/fuse、卸载工具、可选验收工具、挂载表和已有存储锁，并给出中文处理建议。挂载表诊断不读取可能卡住的挂载目录，也不解析挂载点软链接；存在挂载记录不代表守护进程响应正常。启用环境检查后，缺少 fio 等可选工具显示 warning，不会拒绝正常文件操作。

### 兼容性报告与已确认边界

`tests/compatibility.py` 在 direct/cached 两种模式分别运行真正的 Git init/add/commit/checkout/fsck、rsync 更新/删除、反复 Vim 保存、并发读与原子替换、本地 POSIX 记录锁、错误码和打开后删除场景。cached 模式还检查 mmap 同步后建立快照。报告严格区分 pass / fail / unsupported / environment_skip。设置 TEAMFS_REQUIRE_TOOLS=1 会将缺少工具造成的跳过反映为非零退出；硬链接、xattr、默认 direct 模式的共享 mmap 明确列为 unsupported，不算通过。

**SQLite 应用有重要边界：默认 DELETE 日志模式不保证数据库 commit 返回后立即强制结束 TeamFS 仍保留该次提交。** SQLite 将删除回滚日志作为提交步骤，而 TeamFS 普通删除等待下次同步；旧日志可能在重挂载后再次出现，导致 SQLite 回滚已返回成功的应用提交。这个现象已经由自动测试重现，不应把 TeamFS 宣称为任意数据库的透明替代磁盘。

当前验证通过的两种使用方式：SQLite 设置 journal_mode=PERSIST、synchronous=FULL；或者在 DELETE 模式完成数据库事务和关闭连接后，再成功执行 teamfs sync，随后才把它作为本项目验证过的持久化边界。PERSIST 也仅声明本次单数据库测试覆盖，不推断 WAL、多数据库事务或所有数据库软件兼容。TeamFS 自己的后端数据库始终存放在挂载点外，这个应用兼容性边界不改变后端事务保存保证。参考：[SQLite 原子提交说明](https://www.sqlite.org/atomiccommit.html)。

### 随机、故障与生命周期实验

随机测试以原生 Linux 临时目录为参照，在两种 I/O 模式各执行 3 个固定种子、每个 180 次操作，共 1,080 次生成操作；逐步比较创建、偏移写、追加、截断、权限、改名、删除、目录操作的结果及原始路径/内容。中间插入同步、快照和 SIGKILL 重挂载，核对最后成功保存点与只读历史。时间戳及 inode 数字不参与原生目录对照；历史视图权限按只读规则比较。每组生成的原始操作序列保存为 artifacts/random-*.jsonl，失败也保留，含种子、路径字节和写入内容，可用命令重现。

故障矩阵在独立数据库中，对 blobs、nodes、metadata 写入分别注入事务失败，组合普通同步、创建快照、清空回收站，共 9 组；检查失败回滚、dirty、错误状态，杀进程后恢复旧提交，再验证修复和重试后的新提交。另有 120 轮打开/删除/清理/关闭实验，记录句柄、保留内容和重新挂载后的回收结果。内核 lookup 引用仍可能保留已关闭的无路径节点，不能据此直接判断为内存泄漏。这些是有界实验，不是断电或长期生产可靠性证明。

### 耗时与性能基线

只读 `.teamfs/metrics.json` 每次打开生成固定 JSON；读取自身不改变业务计数、dirty 或指标。status.json 中 performance 提供同一组累计统计，runtime 提供打开句柄、等待释放的历史和业务 inode 引用状态。真实监控页面增加耗时表和最小耗时筛选，记录详情可查看 timing_us。

- 每种操作统计本次挂载的次数、错误、累计耗时、最大耗时及锁等待/业务处理/保存累计耗时。
- P50/P95/P99 采用该操作最近 256 次样本的 nearest-rank 计算；slowest 保留本次挂载最慢 20 项。这只是诊断数据窗口，不增加业务文件限制。
- total_us 从进入用户空间回调开始；lock_wait_us 为取得服务锁的等待，service_us 为业务处理，commit_us 是其中的保存子阶段（含树导出和 Store.save）。不包含内核排队和回复传递；内部管理/提交阶段已持锁，锁等待字段为 0。嵌套 commit 与外层 fsync/管理命令不能相加作为总请求时间。
- 成功 lookup/getattr/forget 和监控通道普通访问仍不记录；监控不是所有系统调用的完整追踪。

benchmark-v6 使用原生 WSL Linux 目录、TeamFS direct 和 cached 三组独立路径，运行 fio 顺序写及 CRC32C 验证、4KiB 随机读、4 作业读取；另测 150 个小文件的创建/遍历/删除，以及 4KiB 小文件和 16MiB 文件各修改 4KiB 后的保存成本。每项默认 3 次，保存原始 fio JSON、样本及中位数。文件日志和 trace 关闭，运行时指标开启；不清空宿主页缓存，结果包含热缓存影响，debug 构建不能推断发布版或物理设备性能。SQL 插入字节不是设备写入量。150ms SQLite 写锁演示单独标记，排除在吞吐样本之外。

本次 WSL2 / Rust 1.75 / fio 3.39、debug 构建的三次样本：direct 模式中，4KiB 文件修改后同步命令中位数约 19.26ms；16MiB 文件只修改 4KiB，同步约 86.59ms。对应插入内容分别为 4KiB 和 16MiB。这确认了当前整文件版本保存的成本边界，不是跨系统通用性能结论。完整数据及 cached/native 参照见 baseline-v6.json。

fio 可通过发行版安装，或将 TEAMFS_FIO 指向独立可执行文件。当前实测使用从官方 fio-3.39 标签构建的程序（提交 a6e474c9e896e4ba1eb40066a03402afb040710a）；缺失 fio 时报告 environment_skip，其他基线仍可运行。

结果文件：artifacts/compatibility-v6.json、sqlite-boundary-v6.json、randomized-v6.json、v6-acceptance.json、baseline-v6.json、metrics-example.json、check-example.json。GitHub Actions 工作流固定 Rust 1.75；FUSE 设备不可用时保存明确的环境跳过报告，绝不将未执行的挂载测试算作通过。工作流文件已提供，本地验证不代表 GitHub 云端运行成功。

设计参考：[fuser 示例与测试](https://github.com/cberner/fuser)、[pjdfstest](https://github.com/pjd/pjdfstest)、[JuiceFS 兼容性](https://juicefs.com/docs/community/posix_compatibility/)、[CrashMonkey](https://github.com/utsaslab/crashmonkey)、[fio](https://github.com/axboe/fio)。当前已实现针对 TeamFS 的原生目录对照和故障矩阵，没有声称已通过 pjdfstest/xfstests 全套或直接运行 CrashMonkey 内核模块。

## 一、先体验完整案例

在 Windows PowerShell 执行：

```powershell
wsl -d Ubuntu-24.04 --cd "/mnt/d/PyProject/operating system/Teamwork" -- bash scripts/showcase.sh
```

它在自己的临时挂载点和临时存储中演示：创建报告、自动同步、强制结束进程并重挂载、建立快照、修改和比较、误删新笔记、回收恢复、改名覆盖保护、清理并重挂载。随后演示整目录预览/恢复、独立备份校验、新存储导入以及增量提交。结束时清理演示挂载和临时数据。

- `artifacts/showcase-output.txt`：真实命令输出。
- `artifacts/showcase-trace.log`：真实 FUSE 回调。
- `artifacts/showcase-metrics.json`：单次样例的同步与快照命令耗时、自动保存观测等待时间、逻辑内容大小、SQLite 实际文件占用及恢复内容 SHA256；不能据此推断普遍性能。
- `artifacts/showcase-v4-output.txt`、`artifacts/showcase-v4-metrics.json`：目录恢复、备份和按需读取的真实演示记录。
- `artifacts/performance-v4.json`：0.3 与 0.4 使用同一数据样例的实测对比。
- `artifacts/showcase-diff.json`：实际生成的快照差异。
- `demo/index.html`：离线交互讲解，包含保存与恢复实验、新版删除保护实验和原版内存请求流程。页面是模拟，不连接真实挂载。

原来的内存重置演示仍可运行：`bash scripts/showcase-memory.sh`。它保存独立的 `memory-showcase-*` 记录。

## 真实监控：看见文件系统实际运行

最方便的体验方式是在 PowerShell 执行：

```powershell
wsl -d Ubuntu-24.04 --cd "/mnt/d/PyProject/operating system/Teamwork" -- bash scripts/monitor-demo.sh
```

脚本创建自己的临时挂载与存储，产生一组真实文件操作并启动监控。打开 **http://127.0.0.1:8766**，再对终端输出的挂载目录执行普通文件操作即可观察更新。此页面连接真实 FUSE 状态，原 `demo/index.html` 的原理模拟仍独立保留。Ctrl+C 停止这个演示时，卸载并清理它自己的临时目录。

监控你自己启动的挂载点，在另一个 Ubuntu 终端执行：

```bash
bash scripts/monitor.sh "$HOME/teamfs-mount"
# 自定义端口：bash scripts/monitor.sh "$HOME/teamfs-mount" --port 8767
```

监控服务只监听 127.0.0.1，每秒读取 `.teamfs/status.json` 和 `.teamfs/events.json`，只提供只读接口；停止监控服务不会卸载已有文件系统。页面显示保存状态、内容占用、实际读写速率、缓存、历史记录、操作列表，并支持路径/操作/结果/PID 筛选、记录详情和导出当前筛选的 JSON。采样失败或超时会明确标记断开，保留最后成功采样并标明时间；重新挂载后自动识别新会话。

### 结构化日志

```bash
"$TEAMFS" logs "$MOUNT"
"$TEAMFS" logs "$MOUNT" --errors --path report
"$TEAMFS" logs "$MOUNT" --operation write --json
"$TEAMFS" logs "$MOUNT" --since 100 --json
cat "$MOUNT/.teamfs/events.json"
```

每条记录含格式版本、挂载会话 ID、递增序号、时间、来源、操作、PID/UID/GID、inode、原始路径字节和可读路径、改名目标、操作参数、实际读写字节、结果、errno、错误文本、耗时及操作后的 dirty。同步记录另含触发来源和实际存储错误；管理请求重复 fsync 不会重复执行或重复生成管理记录。

记录范围包括到达 TeamFS 的业务 open/read/write、目录操作、属性修改、失败的路径查询、挂载内管理命令和同步。成功的 lookup/getattr/forget 不进入结构化列表；状态、日志、控制通道的普通读取不重复记入日志，防止监控制造自己的流量。业务正文从不写进日志。PID 是进程身份，不是组员身份；一次编辑器保存可能产生多条请求。“路径不存在”等失败也可能只是应用检查可选文件，不能一概当成应用故障。

默认持久化模式异步写入 `<存储目录>/logs/events.jsonl`，可通过 `--audit-log <外部路径>` 改位置，或 `--no-audit-file` 仅关闭文件日志。内存模式默认只保留运行中的最近记录，也可指定外部日志路径。日志路径必须在挂载点外，防止写日志再次触发日志。

日志是尽力记录的诊断数据，不是不可篡改审计证据或业务持久化保证。内存保存最近 2,048 条，文件按约 8 MiB 自动轮换为当前文件及 `.1`、`.2`；页面显示最近 500 条。这些仅管理诊断数据，不限制业务文件操作。文件写入使用独立线程和非阻塞入队，磁盘不可写或队列繁忙时保留业务操作结果，并显示未写入数量和日志错误；恢复后继续写入。窗口移出和游标缺口会明确显示，不伪装成完整历史。正常退出尝试排空日志，异常退出仍可能缺少最后几条。

内核提前拒绝的权限请求、未到达 TeamFS 的应用/内核缓存操作，以及离线备份导入不在回调日志覆盖范围内。备份创建前的同步可以被记录。监控不读取 SQLite、不调用修改命令；日志文件不包含在业务备份中。新版本无须升级数据库格式，仍为格式 3。

## 二、保存规则

| 行为 | persistent（默认） | memory |
| --- | --- | --- |
| 普通写入、改名、删除、恢复 | 先改内存，标记 dirty | 改内存 |
| 普通文件或目录 fsync、`teamfs sync` | 事务保存整个当前文件树 | 确认当前内存状态，没有磁盘保障 |
| 创建 / 删除快照、清理回收站 | 同时保存当前树、快照集合与回收记录，成功后可跨重启保留 | 仅保留在当前进程 |
| 开启 --auto-sync 后到达间隔 | dirty 时尝试事务保存全部状态，失败后重试 | 不支持该参数 |
| 正常卸载 | 显式保存，失败时进程返回非零 | 进程结束，数据释放 |
| SIGKILL 等异常结束 | 恢复最后一次成功提交，未同步修改丢失 | 全部丢失 |

**关闭文件不是持久化保证。** `flush` 检查句柄，`fsync` 才同步；编辑器是否自动调用 fsync 取决于编辑器。需要明确保存时运行 `teamfs sync`；也可用 `--auto-sync 30` 开启自动保存。自动同步不是自动快照，错误修改也会被保存。这里的同步命令指本程序的管理命令，不承诺系统全局 `sync` 在 direct_io 模式下具有相同效果。

普通文件读取会更新 atime，因此也可能使 dirty 变成 true；状态文件和控制文件访问不会计入业务读写或修改业务树。dirty 表示内存元数据/内容可能尚未提交，不仅表示有内容编辑。

SQLite 使用 WAL、`synchronous=FULL` 和事务保存。保存事务先通过 BEGIN IMMEDIATE 取得写事务，短暂占用会按既有 2 秒 busy timeout 等待；超时仍报告失败。同步失败不会清空当前内存修改，也不会替换上一次完整提交；查看状态中的 `last_sync_error` 后修复存储问题，再重新同步。仅保证成功提交的状态，不承诺磁盘损坏、断电硬件故障或多个应用操作组成一个整体事务。

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

数据库格式为版本 3。首次打开格式 1 或 2 的有效数据库时，在进程锁内自动事务迁移，保留全部当前文件、快照和回收记录；迁移失败回滚，旧版程序不能读取迁移后的数据库。状态 JSON 的 schema_version 仍为 1，新字段为兼容性追加。

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
teamfs mount <挂载点> [--store <目录> | --memory] [--auto-sync <秒数>] [--capacity-mib <MiB>] [--max-file-mib <MiB>] [--cached-io] [--audit-log <外部路径> | --no-audit-file] [--trace]
teamfs logs <挂载点> [--errors] [--operation <操作>] [--path <片段>] [--since <序号>] [--json]
teamfs sync <挂载点>
teamfs snapshot create <挂载点> <名称>
teamfs snapshot list <挂载点>
teamfs snapshot delete <挂载点> <名称>
teamfs restore <挂载点> <快照名称> <源相对路径> <目标相对路径>
teamfs snapshot diff <挂载点> <快照名称> [--json]
teamfs trash list <挂载点> [--json]
teamfs trash restore <挂载点> <记录ID> <目标相对路径>
teamfs trash purge <挂载点> <记录ID>
teamfs trash purge <挂载点> --all
teamfs restore-tree <挂载点> <快照> <源目录或 .> <新目标目录> [--dry-run] [--json]
teamfs backup create <挂载点> <新备份目录>
teamfs backup verify <备份目录>
teamfs backup import <备份目录> --store <新存储目录>
```

单文件恢复接受普通文件或软链接；源和目标都相对于文件系统根目录。目标父目录必须存在，目标必须尚不存在。禁止绝对路径、`.`、`..`、空路径分量及 `.teamfs` 管理区。复制文件内容（链接则复制目标字符串）、权限和 mtime，分配新 inode；恢复后仍需同步或正常卸载。

快照名称为一个合法 UTF-8 文件名，支持中文；重复名称报错。最多 10 个快照，快照内容合计最多 128 MiB。超限拒绝，不自动清理历史。需要删除时显式运行 `snapshot delete`，已打开的历史文件仍可读到关闭。

普通 `cat`、`diff`、`cp` 可以访问快照目录。普通 `cp` 是否覆盖目标取决于 cp 参数；**默认不覆盖是 `teamfs restore` 的保证**。快照是本机历史保存点，不是异地备份。

正常卸载（在项目目录）：

```bash
bash scripts/unmount.sh "$MOUNT"
```

该脚本先显式同步；保存失败会退出并保留挂载。直接 `fusermount3 -u` 时，守护进程也会在挂载循环结束后保存，但调用者应查看挂载终端的退出结果。异常退出后的失效挂载可用 `fusermount3 -u <挂载点>` 清理，再以同一存储重启。

### 回收站：删除和改名覆盖保护

普通文件被 `rm` 删除，或成为 `rename` 的旧目标时，TeamFS 保存其操作前的内容和属性，再移除原目录入口。直接覆盖写入、截断不生成回收记录，需用快照保护。目录本身不进回收站；`rm -r` 逐个保护文件，可能部分成功。整目录恢复使用预先建立的快照。

```bash
printf '没有建快照的新笔记\n' > "$MOUNT/notes/new.txt"
rm "$MOUNT/notes/new.txt"
"$TEAMFS" trash list "$MOUNT"
# 用实际清单中的 ID 替换 1；内容也可直接 cat
cat "$MOUNT/.teamfs/trash/1"
"$TEAMFS" trash restore "$MOUNT" 1 notes/new-recovered.txt
"$TEAMFS" sync "$MOUNT"
"$TEAMFS" trash purge "$MOUNT" 1
```

- 上限为 **1,024 条、64 MiB 内容**，独立于当前树和快照限额。空文件占一条；满时删除或替换返回 ENOSPC，原文件保持不变。先清理记录再重试，不自动淘汰。
- 每次记录有独立 ID，清理后已提交的 ID 不复用。记录包括原始路径字节、删除时间、unlink / rename_replace 原因、属性和大小。
- 删除与对应记录一起在下一次成功同步中提交；未同步就异常退出，两者一起回到旧提交。原打开句柄仍可写，回收副本固定为删除时的内容。
- 恢复只复制普通文件或软链接到不存在的新路径，父目录须已存在，保留内容、权限、uid、gid、mtime。原记录保留，恢复结果等待后续同步。
- `purge` 成功意味着清理及全部当前修改已提交；失败保留记录。`--all` 明确清空全部活动记录；已打开的历史文件仍可读到关闭。
- 回收文件只读。被清理但仍被句柄或内核引用持有的数据暂留内存，不计入活动回收站容量。SQLite 空闲页可复用，清理不保证数据库立即缩小。

### 可选自动同步

```bash
bash scripts/start.sh "$HOME/teamfs-auto" --auto-sync 30
```

间隔必须是 **1～86,400 的整数秒**，不指定则关闭，与 `--memory` 互斥。挂载成功后独立定时线程在空闲期间也会检查 dirty，通过同一个服务锁保存；没有修改则跳过。失败保留内存状态，记录 last_sync_error，下周期重试。退出时立即唤醒并停止定时线程，再执行正常退出保存。

时间间隔不是最大丢失时间保证：保存可能等待其他操作、执行耗时或失败。读取业务文件更新 atime，也可能触发下一次自动保存。它保存当前状态，不产生历史快照。

### 快照差异清单

```bash
"$TEAMFS" snapshot diff "$MOUNT" before-edit
"$TEAMFS" snapshot diff "$MOUNT" before-edit --json
cat "$MOUNT/.teamfs/diffs/before-edit"
```

比较快照与当前业务树，按原始路径排序，列出新增、删除、修改和类型变化。文件内容逐字节比较；属性比较 mode、uid、gid、文件 mtime；忽略 atime、ctime、inode、链接数和目录 mtime。改名呈现为一删一增，新增/删除目录也列出子项。排除整个 `.teamfs`，查询不改变业务读写计数、atime 或 dirty。

JSON 含 summary、changes、path_bytes、path_display、content_changed、metadata_changed 和前后属性；终端转义控制字符、非 UTF-8 字节及反斜杠。成功比较退出 0，无论有无差异；缺失快照或执行错误返回非零。每次打开结果固定，分段读取一致；快照删除后已打开的结果仍可读完。

### 整目录恢复与预览

```bash
"$TEAMFS" restore-tree "$MOUNT" before-edit notes notes-recovered --dry-run
"$TEAMFS" restore-tree "$MOUNT" before-edit notes notes-recovered
# 源目录 . 表示快照中的整个业务根目录，排除 .teamfs
"$TEAMFS" restore-tree "$MOUNT" before-edit . all-recovered --dry-run --json
```

预览列出目标路径、文件/目录/软链接数和内容大小，以及 can_restore / error_errno。目标必须不存在、父目录须存在，容量不足或路径冲突时拒绝整个恢复。实际执行重新检查条件，在内存中建立完整子树后一次性接入目录，不留下半成品，也不影响已有打开句柄。新节点有新 inode，保留内容、权限、uid/gid 和 mtime，结果等待后续成功同步。

源和目标的中间目录不跟随软链接。恢复软链接只复制其目标字符串，不复制或遍历目标；绝对链接和指向目录外的链接保留原义，文件系统不是沙箱。回收站恢复仍按记录逐个进行，不把不同时刻的删除记录猜测为同一目录版本。

### 独立备份、校验与导入

```bash
"$TEAMFS" backup create "$MOUNT" "$HOME/teamfs-backup-01"
"$TEAMFS" backup verify "$HOME/teamfs-backup-01"
"$TEAMFS" backup import "$HOME/teamfs-backup-01" --store "$HOME/teamfs-restored-store"
bash scripts/start.sh "$HOME/teamfs-restored" --store "$HOME/teamfs-restored-store"
```

备份目录包含独立的 state.sqlite3 和带 SHA256、长度、格式及时间的 manifest.json。创建时先显式同步，再通过 SQLite Online Backup API 取得一致的已提交状态；复制后清理无引用内容并验证数据库。后台发生后续提交时，备份仍是一致状态，但不承诺与首次 sync 的时刻完全相同。尚未从应用/内核缓存写入 TeamFS 的内容需先 fsync/msync。

备份必须位于挂载点和原存储目录之外；备份和导入均要求目标目录不存在，父目录已存在。结果先在私有临时目录中完成，通过不可覆盖的原子改名发布。导入校验实际复制的字节及文件树结构，失败不发布半成品。SHA256 检测意外损坏，不是防篡改签名。备份放到另一块磁盘或设备，才可覆盖原设备故障的风险。

### 容量、缓存与软件兼容性

```bash
bash scripts/start.sh "$HOME/teamfs-large" --capacity-mib 256 --max-file-mib 32
# 需要内存映射读写时，显式开启内核缓存模式
bash scripts/start.sh "$HOME/teamfs-cached" --cached-io
```

总容量可配置为 1～4096 MiB，单文件为 1～64 MiB，单文件不得超过总量；不指定时使用已有存储中保存的配置，新存储默认 64/16 MiB。新配置低于已有数据时拒绝。快照仍限制 10 个/128 MiB，回收站仍限制 1,024 条/64 MiB，不能通过提高业务容量绕过历史容量检查。读取缓存有固定上限，未同步文件内容和元数据另外占用内存，不能将 8 MiB 理解为整个进程的内存上限。

默认 direct_io 便于精确观察回调；可选 --cached-io 仅对业务文件开启内核页缓存，管理和历史文件仍使用 direct_io。实际验证覆盖 Vim 编辑保存、cp -a、tar 打包解包、相对/悬空软链接、owner 权限、内核本地 flock、四进程追加写，以及 cached 模式的共享 mmap、msync/fsync 和重挂载。应用缓冲区和未刷新的 mmap 内容不属于 TeamFS 已收到的写入；建立快照或备份前应先完成应用保存。

兼容性以这些已验证场景为范围，不宣称完整 POSIX 或所有软件兼容。硬链接、xattr、多用户协作仍不支持；默认模式没有开启 mmap。

## 五、虚拟文件与状态字段

```text
挂载点/
├── welcome.txt
├── notes/
└── .teamfs/
    ├── status.json
    ├── metrics.json
    ├── events.json
    ├── control
    ├── snapshots/
    │   └── before-edit/
    │       ├── welcome.txt
    │       └── notes/
    ├── trash/
    │   ├── index.json
    │   └── <记录ID>
    └── diffs/
        └── before-edit
```

`.teamfs` 是保留目录。历史树不包含管理目录，不能形成递归快照。快照文件与当前文件的 inode 不相同，历史数据只读。

`status.json` 每次打开生成一份固定 JSON，在同一句柄中分段读取始终一致；重新打开刷新。字段含义：

| 字段 | 含义 |
| --- | --- |
| schema_version / filesystem / version / mode | 状态格式版本、系统名、程序版本和模式 |
| uptime_seconds | 当前挂载运行时长 |
| files / directories | 当前有路径的业务文件（含软链接）和目录数，目录包括根目录，排除管理树 |
| used_bytes / capacity_bytes / max_file_bytes | 当前内容占用（含仍打开的已删除文件）、默认 64 MiB 总上限、16 MiB 单文件上限，可通过挂载参数调整 |
| snapshot_count / snapshot_bytes | 活跃快照数量和逻辑文件内容字节总量，重复内容按份计数 |
| snapshots | 名称和创建时间列表 |
| read_calls / write_calls / read_bytes / write_bytes | 本次挂载成功的业务回调次数和实际字节数，含快照和回收文件读取，排除状态、清单、差异与控制请求；不是 shell 命令数 |
| trash_count / trash_bytes / trash_limit / trash_capacity_bytes | 活动回收记录数、内容字节、1,024 条与 64 MiB 上限 |
| auto_sync_interval_seconds / auto_sync_attempts / auto_sync_successes | 自动同步间隔（关闭为 null）、本次挂载实际尝试次数与成功次数；clean 检查不计入尝试 |
| cache_bytes / cache_limit_bytes / content_fetched_bytes | 用户空间读取缓存占用、8 MiB 上限及本次从 SQLite 获取的内容字节 |
| resident_unsaved_content_bytes | 当前及历史、仍打开的无路径文件持有的未落盘内容，去除同一版本重复引用 |
| last_commit_rows / last_commit_content_bytes | 上次事务改变的业务/历史元数据行数、插入的内容字节；不等于设备实际写入量 |
| store_path_bytes | 持久化数据库的原始路径字节，供只读备份客户端定位；内存模式为 null |
| audit | 挂载会话、最近事件窗口、累计成功/未成功次数、文件日志写入/丢失及错误状态 |
| dirty / last_sync_unix / last_sync_error | 未同步标记、最近成功同步 Unix 秒时间戳、最近同步错误 |

目录对象、索引、SQLite 页、WAL 以及句柄占用不包含在逻辑容量中，实际磁盘/内存占用会更大。删除快照后 SQLite 可以复用空闲页，数据库文件不保证立即缩小。

`.teamfs/control` 为管理客户端使用的控制文件：一个句柄缓冲一个最多 64 KiB 的 JSON 请求，显式 fsync 时执行一次。重复 fsync 返回同一结果，关闭不执行；格式无效不改变状态。命令通过挂载点进入同一个服务层，不直接写数据库。JSON 路径使用字节数组，支持非 UTF-8 文件名。旧的只写客户端仍兼容；目录恢复客户端使用读写句柄，在 fsync 执行后读取固定的预览/结果 JSON，未执行前读取返回 EAGAIN。

## 六、代码结构与实现决策

| 模块 | 职责 |
| --- | --- |
| src/main.rs | CLI、挂载检查、控制请求、正常卸载后的显式同步 |
| src/filesystem.rs | fuse-rs 回调适配、日志、reply |
| src/model.rs | inode、目录项、文件字节、句柄、引用回收、树导入导出 |
| src/service.rs | 保存边界、快照、虚拟 inode、状态文件和控制句柄 |
| src/store.rs | SQLite 格式校验、版本迁移、进程锁、事务读取与提交 |
| src/history.rs | 不可变回收记录、原始路径转义、快照差异比较 |
| src/autosync.rs | 可唤醒停止的定时线程，通过服务锁触发同步 |
| src/content.rs | 不可变内容版本、SQLite 按需读取、8 MiB 页面缓存 |
| src/backup.rs | 一致备份、SHA256 校验及新存储原子导入 |
| src/metrics.rs | 请求分阶段耗时、最近样本分位数和慢请求 |
| src/diagnostics.rs | 只读离线检查与环境/挂载诊断 |
| src/audit.rs | 结构化事件窗口、异步 JSONL 写入、轮换与错误隔离 |
| scripts/monitor.py | 只读采样桥接，提供 localhost 页面与 API |

业务树导出只保留有路径的节点，句柄、lookup 引用、运行计数器和已 unlink 的无路径节点不会恢复到下次挂载。数据库保存版本、当前树、快照树、原始文件名字节和属性。启动会检查父子关系、重复名称、容量和链接数。

快照固定在创建请求执行时的状态；不会把多次 write 或整个编辑器保存过程自动识别成应用事务。先完成编辑，再主动创建快照。

0.4 在事务内比较元数据，仅插入、更新或删除发生变化的节点。内容存放在不可变 blobs 中；快照、恢复和回收记录复用已有内容版本，修改后采用新的内容版本，不做哈希去重。提交成功后才把内存内容切换为磁盘引用，失败保留未同步修改。所有请求与定时同步仍通过 `Arc<Mutex<Service>>` 串行执行，保存会阻塞其他请求。

挂载只加载业务及历史元数据，不加载全部文件内容。读取按 64 KiB 页从 SQLite 获取，LRU 缓存上限 8 MiB；未同步内容不能淘汰，第一次修改一个已保存文件时载入该文件，后续独占写入原地修改。增量粒度是整个发生变化的文件，并非文件块。逻辑容量和历史容量限制继续生效。被删除的历史版本可能被打开句柄持有，因此无引用 blobs 在下次启动时清理；数据库文件大小不会立即缩小，独立备份使用 VACUUM 生成紧凑副本。

## 七、验收与边界

```bash
export CARGO_TARGET_DIR="$HOME/.cache/teamfs/target"
cargo test --locked
bash scripts/smoke.sh
bash scripts/test-v2.sh
bash scripts/test-v3.sh
bash scripts/test-v4.sh
bash scripts/test-v5.sh
bash scripts/test-v6.sh
bash scripts/showcase.sh
# 或用 bash scripts/check.sh 一次运行全部后端验收
```

`smoke.sh` 使用内存模式复验原有 12 个挂载场景和 1 个重挂载场景。`test-v2.sh` 使用独立存储与挂载点，验证 15 组新增场景，包括 SIGKILL、只读快照、历史句柄、恢复、状态分段读取、控制请求幂等、SQLite 事务失败注入、重试、容量限制和损坏数据库拒绝。

现有 7 个模型测试之外，新增树导入校验、原始名称持久化、无路径节点排除、历史节点回收和控制请求边界测试，再加上路径转义、差异规则及回收/差异句柄回收，再加上耗时统计和挂载路径解析，共 20 个测试。实际结果见 `artifacts/model-tests.txt`、`artifacts/acceptance.txt`、`artifacts/v2-acceptance.txt`、`artifacts/v3-acceptance.txt`、`artifacts/v4-acceptance.txt`、`artifacts/v5-acceptance.txt`。每次测试重写对应记录。

`test-v3.sh` 验证删除与替换保护、两种容量限制、恢复、清理失败回滚、空闲自动保存及重试、差异、迁移与联合场景。设置 TEAMFS_V02_BIN 为旧版 0.2 可执行文件时，还额外验证由旧程序实际生成的存储；本次实测包含这一项，共 15 组。常规版本 1 迁移及失败回滚测试始终执行。

`test-v4.sh` 本次通过 11 组目录恢复、备份、真实软件、缓存与增量保存场景，包含由旧版 0.3 生成的格式 2 存储迁移。设置 TEAMFS_V03_BIN 指向旧程序时启用该附加迁移检查。

`test-v5.sh` 检查真实请求字段、控制请求幂等、读取无自循环、固定 JSON、CLI 筛选、同步错误、文件日志故障隔离及恢复、轮换、HTTP 只读接口、断开/重挂载与会话游标。样例记录见 artifacts/events-example.json。

所有测试和完整演示都使用自己创建的挂载点，不会操作既有 `teamfs-playground`。旧进程不会因源码更新自动升级。

当前仍为单用户教学实现，使用 default_permissions，未开启 allow_other。没有实现硬链接、扩展属性和完整 POSIX 兼容、自动快照、多用户协作、网络存储或硬件故障恢复。初期异常中断导致未完成初始化的数据库会被拒绝，需明确选择新的空存储目录，不能把它当成已有可靠存储使用。

### 一次相同样例的版本对比

样例：4 个 8 MiB 文件、1 个 4 KiB 文件、预置文件、两个快照；只修改 4 KiB 文件，连续测量三次。

| 指标 | 0.3 | 0.4 |
| --- | ---: | ---: |
| 同步命令耗时中位数（含客户端开销） | 0.5538 s | 0.0768 s |
| 每次向 SQLite 插入的内容字节 | 100,676,829 | 4,096 |
| 重挂载后的进程 RSS | 128.13 MiB | 8.64 MiB |
| 卸载后的后端文件占用 | 96.15 MiB | 32.20 MiB |

这是本机单一样例，不是普遍性能结论。内容字节由 SQLite 插入触发器实测，不等于设备写入量；RSS 包含程序和 SQLite 开销。详细环境与各次数据见 artifacts/performance-v4.json。可通过 TEAMFS_V03_BIN 指定旧版程序后运行 bash scripts/benchmark.sh 复现；该基线程序不随源码仓库分发。

## 八、答辩叙事与资料

项目基于 fuse-rs 开发应用，扩展的是 TeamFS 的存储与恢复能力，没有修改 fuse-rs 库。建议围绕同一份报告解释：普通读写如何到达 Rust、保存点怎样形成、快照如何隔离、错误发生后如何用差异清单定位变化，如何从快照或回收站取回内容。

展示实测记录时说明环境、样例大小及模式。20 个单元测试和真实挂载场景用于证明行为；单次 showcase 的耗时只代表该样例，不能宣称普遍性能优势。PPT 尚未制作，输出和测量记录可作为后续素材。

参考：[fuse-rs 官方仓库](https://github.com/zargony/fuse-rs)、[固定版本 API](https://docs.rs/fuse/0.3.1/fuse/)、[Linux FUSE](https://docs.kernel.org/filesystems/fuse/fuse.html)、[Rust 官方教程](https://doc.rust-lang.org/book/)、[任务参考文章](https://blog.csdn.net/gitblog_00096/article/details/138896027)。

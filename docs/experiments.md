# 科研实验的文件输入追溯

目标是回答“这次运行实际访问了哪些挂载内输入、对应什么内容版本、以后能否取回”。不是仅保存整个目录，也不把普通诊断日志冒充完整依赖清单。

## 一键真实演示

在 WSL 的项目目录运行：

```bash
bash scripts/demo-experiment.sh
```

脚本创建自己的临时挂载点和存储，复制 `examples/research`，执行真正的 Python 分析程序。第一次使用三个样本 10、20、30，配置倍率 2，校准系数 1.5，结果为 41.5；同名样本改为 50、60、70 后结果为 121.5；随后按第一次报告重放旧输入，核对结果逐字节一致及 SHA256。目录中有一个未使用文件，不应列为正文输入。

报告、stdout/stderr、结果文件以及独立数据库备份保存在 `artifacts/experiment-demo/<独立演示目录>/`，最新位置写在 `artifacts/experiment-demo-latest.json`。演示结束清理自己的挂载与临时存储，不停止其他挂载。归档报告中的原临时目录路径用于记录现场；重新导入备份并给 verify/replay 指定新挂载点即可使用保留的输入版本。

备份先在 WSL Linux 文件系统上完成原子发布和验证，再复制到 Windows 项目目录并重新验证 SHA256。直接向 `/mnt/d` 创建备份可能因共享文件系统不支持目录同步或 renameat2 而失败；正式报告/备份目标建议使用 WSL home 下的新目录。

## 用自己的程序运行

先准备完整输入版本。默认 direct 模式下，成功完成普通文件写入后可直接开始；cached 模式下，先让写入程序对文件执行 fsync/msync，以发布内核尚未提交的写入。快照固定的是开始时服务看到的文件树，不代表多个外部写入者的应用事务已经完成。

```bash
TEAMFS="$HOME/.cache/teamfs/target/debug/teamfs"
MOUNT="$HOME/teamfs-mount"

"$TEAMFS" experiment run "$MOUNT" analysis-01 \
  --output "$HOME/analysis-01-report" -- python3 analyze.py

"$TEAMFS" experiment show "$HOME/analysis-01-report"
"$TEAMFS" experiment verify "$MOUNT" "$HOME/analysis-01-report"

# 同名当前文件已经改变时，复用旧输入快照和报告内的原命令
"$TEAMFS" experiment replay "$MOUNT" "$HOME/analysis-01-report" analysis-01-replay \
  --output "$HOME/analysis-01-replay-report"

# 将完整旧输入树复制到新的业务目录，不覆盖；恢复后需同步
"$TEAMFS" experiment restore "$MOUNT" analysis-01 recovered-inputs
"$TEAMFS" sync "$MOUNT"
```

命令的工作目录是输入快照根目录。`TEAMFS_INPUT` 是该目录的绝对路径，`TEAMFS_OUTPUT` 是新报告目录下的 `outputs/`。输出必须写到后者或其他挂载点外位置；输入快照只读。程序不需要接入日志 SDK，普通 open/read 即可。报告目录必须是新目录且位于挂载点和后端存储之外，已有目录不会覆盖。

程序退出码保留。非零退出也记录输入证据、stdout/stderr 和错误状态，不算成功实验。SIGINT/SIGTERM 会停止该进程组并保存结束记录；报告标记 cancelled，不能作为正常完成实验自动 replay。主命令退出时终止仍在同组中的后台进程，避免采集结束后继续读输入。

## 原理与报告语义

1. 运行器先启动一个独立进程组的执行门控程序。用户命令尚未运行。
2. 管理请求通过 `.teamfs/control` 创建并提交 `experiment.<名称>` 快照，再注册该进程组的采集会话。失败时不释放执行门，不运行用户命令。
3. 释放执行门，命令在快照根目录运行。FUSE 回调在同一 Service 锁下，将同组访问聚合到专用会话；不使用会轮转或丢失的 audit ring/file。
4. 请求按成功正文读取、仅打开文件、元数据访问和错误分类。记录原始路径字节、可读路径、inode 属性、调用 PID、操作次数及实际返回字节。目录项与缺失文件查询也能成为解释实验行为的证据。
5. 结束后对观察到的文件版本计算完整 SHA256，即使程序只读了局部。哈希过程不生成实验的正文读取次数。报告以临时文件 + fsync + rename + 目录 fsync 更新。

`manifest.json` 同时保留原始命令参数字节、可读命令、版本、输入路径、过程状态、退出码/信号和 capture。

- `capture.inputs`：成功打开或读取的普通文件。`content_read` 表示发生正文 read 请求；`opened_only` 不声称正文被使用。空文件可产生返回 0 字节的成功读取。
- `capture.metadata_accesses`：只查属性、目录或链接等访问，不混入正文输入。
- `capture.errors`：固定快照上的失败访问及 errno，例如请求了缺失的配置文件。
- `capture.unpinned_mount_accesses`：同组读取/打开可读的其他业务、快照或回收文件的次数；不静默声称这些输入已经固定。存在这些访问时 verify/replay 拒绝作为全部观察到的挂载内正文输入已固定的报告使用。
- `capture.external_link_accesses`：读取了可能离开固定输入树的软链接。报告保留链接目标，基于快照树解析相对路径和中间链接；绝对链接以及无法可靠解析的循环不会被认证为固定输入，verify/replay 明确拒绝。
- `capture_complete`：本次会话采集是否正常完成，不代表算法成功，也不代表程序的全部外部依赖已经被捕获；必须同时看 phase、exit_code、signal、cancelled 和 scope。

snapshot 名称固定一个不可变树，文件 SHA256 标识正文内容，路径字节避免非 UTF-8 或控制字符名称混淆。replay 复制原输入树到一个新的实验快照，复用原命令；当前业务状态仍按原同步规则一并保存，内容块可共享。

## 留存、中断与边界

报告保存在挂载点外，已成功建立的输入快照随存储同步持久化。运行期间采集状态在 daemon 内存中；daemon 异常退出丢失尚未导出的会话，不宣称追踪结果崩溃安全。报告先保存 prepared/running 状态，后续失败不会伪装成完整实验。

运行器被 SIGKILL 后，主命令通过 Linux parent-death signal 终止；普通子进程并不因此全部保证终止。它们如仍在原组内，daemon 继续观察，但 recover 必须等该组已不再运行。留在系统外、主动离开进程组的进程不在追踪保证内。

若运行器中断、daemon 仍在且原进程组已结束，可保存剩余观测记录：

```bash
"$TEAMFS" experiment recover "$MOUNT" analysis-01 "$HOME/analysis-01-report"
```

它明确输出 interrupted、不完整报告，不补造未知退出码。若 daemon 已重启，运行计数不能恢复，但已提交输入仍可通过 experiment restore 取回。

实验快照不能用普通 snapshot delete 删除，防止误删报告对应版本；完成或显式 recover 后，可用 `experiment delete <挂载点> <名称>` 清理。该操作提交删除结果，外部报告保留，但以后不能从此存储恢复该输入版本。使用原有快照条数与逻辑容量规则，没有新增实验配额。memory 模式只能教学体验，重挂载后输入版本丢失，报告明确标注。

本版只追踪此挂载上的固定快照、此进程组的 FUSE 请求。外部脚本和库、其他磁盘文件、网络输入、环境变量、随机性、GPU 行为与主动离组进程未完整追踪。因此称为“文件输入可追溯”，不称为任意科研程序完全可复现。快照文件使用 direct I/O，mmap 不支持；业务树的 cached-io 配置不改变该规则。read 字节是文件系统返回的字节，不证明算法实际使用了每个字节。

verify 对报告中的文件 SHA256 与对应快照重新比较，不执行实验，也不验证科学结论或完整运行环境。报告及后端由本机用户管理，哈希不是对抗恶意修改的签名。若要长期转移，保留报告和独立 TeamFS backup；报告自身不包含原始正文。

## 验收

```bash
bash scripts/test-experiments.sh
bash scripts/check.sh
```

真实挂载覆盖：读过/未读过的区分、内容哈希、修改后重放、跨重启恢复、中文和非 UTF-8 名称、二进制局部读取、空文件、仅打开/仅属性查询、命令失败、普通子进程、其他进程访问隔离、超过日志窗口的 2500 次读取且并发修改当前文件、未固定输入与越界软链接提示、中断/恢复、daemon 崩溃及启动事务失败。结果保存在 experiment-acceptance.json/txt 与真实 experiments-trace.log。

研究参考：[ReproZip](https://github.com/VIDA-NYU/reprozip) 通过系统调用追踪收集计算依赖；[DVC](https://doc.dvc.org/example-scenarios/versioning-data-and-models) 关联数据、代码与模型版本。TeamFS 当前做的是挂载目录内透明记录和固定输入版本的简化案例，不宣称替代这些系统或提出全新算法。

# 成员 5 独立性能基线

本轮以当前 TeamFS 0.8 为基线，新增独立评估脚本，不修改 Rust、共享测试工具或其他成员的报告格式。目标是先取得可复跑的三组对照和原始证据，供后续功能合并前后比较。它是成员 5 的第一阶段交付，不代表五人分工的全部验收已完成。

## 如何运行

在 Windows PowerShell 中执行：

```powershell
wsl -d Ubuntu-24.04 --cd /mnt/d/操作系统/TeamFS -- bash scripts/benchmark-member5.sh
```

在项目目录的 Linux 终端中执行：

```bash
# 正式基线：release，3 种负载 × 3 种模式 × 3 次 = 27 个样本
bash scripts/benchmark-member5.sh --samples 3

# 冒烟检查，不能替代每组至少 3 次的正式基线
bash scripts/benchmark-member5.sh --samples 1 --workloads small

# debug 必须单独成组，不能与 release 混算中位数
TEAMFS_BENCH_PROFILE=debug bash scripts/benchmark-member5.sh --samples 3
```

依赖为 Linux/WSL2、Python 3.11+、Rust/Cargo、libfuse 2 开发库、pkg-config、C 编译器及 fusermount。当前脚本用 Python 3.12，Rust 1.75。运行时无需 root；首次安装依赖可能需要管理员权限。构建默认使用 `$HOME/.cache/teamfs-member5/target`，不会覆盖原有脚本的编译目录。

每次输出到新的 `artifacts/member5/<UTC 时间及随机后缀>/`，不覆盖旧结果。数据、挂载点、数据库和实验输出在 WSL 的 Linux HOME 临时目录中；Windows 工作区只接收计时结束后的证据副本。成功样本正常卸载后清理其临时输入与数据库；失败时保留日志、报告及必要的 Linux 目录，不修改其他挂载。

计时图使用 Matplotlib，可在安装 `python3-matplotlib` 后对已保存数据生成，不重新测量：

```bash
python3 scripts/plot-member5.py artifacts/member5/<本次目录>
```

图中柱表示中位数，点表示全部样本，输出 PNG 和可编辑 SVG。左右两图分别展示整个命令和程序内部读取/哈希循环，二者存在包含关系，不是可以相加的两个阶段。

## 对照设计

| 模式 | 程序读取位置 | 额外行为 |
|---|---|---|
| native | 原生 Linux 目录 | 无 TeamFS 守护进程 |
| teamfs | TeamFS 当前业务目录 | 默认 direct I/O，无实验采集会话 |
| experiment | TeamFS 固定只读快照 | 使用现有 experiment run，固定输入、采集访问、生成报告 |

三组使用同一个 Python 程序、相同确定性数据、相同读取顺序和 64 KiB 读取缓冲区，均把结果写在挂载点外。所有模式都计算 SHA256，所以本测试是“文件读取加哈希”工作负载，不能称作纯磁盘吞吐测试。

| 负载 | 输入数据 | 读取轮数 | 用途 |
|---|---|---:|---|
| small | 1 × 4 KiB | 32 | 观察短任务和反复打开读取 |
| many | 256 × 4 KiB | 3 | 观察多文件路径解析及访问记录成本 |
| large | 1 × 16 MiB | 3 | 观察较大输入顺序读取与版本校验成本 |

数据是明确标注的合成数据，不作为科研案例。每组还包含程序源码和一个故意不读取的 `unused.bin`。TeamFS 默认附带的示例文件未删除，快照可能包含它们，但它们不应出现在正文输入清单中。

每个样本新建独立挂载与存储，防止快照积累改变后续样本。三轮模式顺序依次为 native/teamfs/experiment、teamfs/experiment/native、experiment/native/teamfs。未清除系统页缓存；输入准备和同一程序多轮读取会产生热缓存，不宣称严格冷读。日志文件、trace 和自动同步关闭，内存日志及运行指标仍开启。

## 测量口径

| 字段 | 含义 | 能否与其他阶段相加 |
|---|---|---|
| mount_ms | 启动及等待挂载可用 | 单独列出，不计入程序执行 |
| input_copy_ms | 向目标复制相同输入 | 与 persist、command wall 不重叠 |
| input_persist_ms | native 对文件和目录 fsync；TeamFS 执行 sync CLI | 两者机制不同，应分别解释 |
| command_wall_ms | 启动子进程到命令返回的外部耗时 | 包含 body；实验模式还包含快照、采集、哈希和报告保存 |
| workload_body_ms | 程序内部打开、读取、SHA256 和关闭循环 | 已包含在 command wall，不能重复相加 |
| verify_cli_ms | 实验完成后另执行 experiment verify 的耗时 | 不包含在 command wall；原生和普通模式为不适用 |
| snapshot_ms / capture_hash_ms / report_export_ms | 当前版本没有独立测量接口 | 填 null，不填 0、不用差值冒充实测 |

本轮不把 `command_wall_ms - workload_body_ms` 命名为“采集耗时”。差值同时含解释器启动、调度、运行器轮询、保存和其他管理开销。代码目前每 30 ms 检查一次子进程退出，短任务的命令耗时会受到影响；本轮保留现有行为，不为改善跑分而修改其他成员的模块。

若需要一个用户视角的“准备并运行”总值，可使用 copy + persist + command wall，并明确不含挂载、额外 verify 和证据归档。不要额外加入 body 或内部指标中的 commit 子阶段。

内存分别记录：

- workload_self_maxrss_bytes：负载进程的 Linux ru_maxrss，含 Python 解释器，非整个进程树之和。
- daemon_rss_before_run_bytes：输入准备后、命令执行前的守护进程 RSS。
- daemon_rss_sampled_peak_bytes：命令期间每约 10 ms 采样的 VmRSS 最大值，可能错过瞬时峰值；不代表精确全生命周期峰值。
- 运行器自身内存暂未独立测量。不要将两类 RSS 简单相加并称为同一时刻的总峰值。

存储分别记录输入准备后、命令完成后及卸载后的数据库、WAL、SHM 文件长度。报告元数据与工作负载输出分开计数。文件长度不等于已分配物理空间，也不等于设备物理写入量；卸载可能触发 WAL checkpoint，所以不能混用时点。

## 证据和正确性

每次保存：

- `summary.json`：环境、提交号、本地修改状态、源文件与二进制 SHA256、全部样本、测量边界。
- `samples.csv`：便于统计的平面数据；`report.md`：各组中位数。
- `source/`：本次测量使用的 Rust 源码、依赖锁及评估脚本副本。
- `fixture-*.json`：输入文件哈希、规模及独立计算的预期输出。
- 各样本目录：命令、stdout/stderr、工作负载输出、实验 manifest、verify 输出、守护进程日志和状态/耗时指标。

本地运行保留完整目录。准备提交证据时，执行 `python3 scripts/package-member5.py artifacts/member5/<本次目录>`，工具核对源码、样本和结果哈希后打包，并逐文件验证 ZIP 内容。Git 保留顶层摘要与 `raw-evidence.zip`，忽略原始子目录；组员从 ZIP 解压即可查看全部原始证据。JSON/CSV 的字节保持规则避免 Windows/Linux 换行转换破坏历史哈希。

每个样本必须与独立预期结果一致。实验模式额外核对完整输入清单、输入 SHA256、正文读取分类、数据文件返回字节数、未使用文件排除、成功退出及无越界输入，并真正执行 verify。失败样本保留错误，不并入中位数；套件遇到失败返回非零。只有这些成功样本的性能数据可用于比较。

## 与其他成员的接口

具体依赖、各成员交付内容、阶段计时提案和更新流程见 [成员 5 对接说明](member5-handoff.md)。提案尚未改动 Rust 实现。

新增的 `scripts/compare-member5.py` 可比较两个已保存的正式评估目录：

```bash
python3 scripts/compare-member5.py artifacts/member5/<旧目录> artifacts/member5/<新目录> \
  --output artifacts/member5-comparisons/<新建对比目录>
```

工具默认拒绝样本不完整、输入输出不同或测量条件不一致的直接比较，保留未测阶段为 N/A。详细规则和例外标记见对接说明。

成员 5 独立维护 `scripts/benchmark-member5.sh`、`tests/member5_benchmark.py`、`tests/member5_workload.py` 和本文档，使用独立的 `artifacts/member5/` 目录。

后续按以下顺序联调：

1. 与成员 2/4 约定 experiment run 的阶段计时字段，再将快照提交、运行等待、采集汇总与哈希、报告保存拆开。字段应基于单调时钟，注明嵌套关系；不能用报告秒级时间戳测短任务。
2. 成员 3 加入中途采集保存后，固定数据和构建方式，比较关闭/开启保存及不同保存间隔的成本，注明证据可靠性差异。
3. 成员 4 提供实际案例后，补充真实输入规模、程序和预期结果，并覆盖其输出摘要功能的成本。
4. 最终合并版本重新跑同一脚本；选择有测量依据的优化，保留前后样本并通过成员 2 的完整回归。
5. 将最终图表、原始样本和测量口径交给成员 6。当前基线不能写成新增功能全部完成后的性能验收。

当前已具备三组对照、原始样本留存、独立正确性核对和资源记录框架；内部阶段拆分、实际案例、采集持久化成本及优化前后对照仍是后续工作。

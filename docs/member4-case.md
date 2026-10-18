# 成员 4：公开数据案例、输出证据与报告比较

本文记录成员 4 的交付：来源明确的公开数据案例、指定输出文件摘要、轻量环境信息、实验报告比较，以及同名输入变化和旧版本重放的完整证据。所有结果产物位于 `artifacts/`，案例代码与数据位于 `examples/research/iris/`。

## 1. 数据来源与可信性

| 项目 | 内容 |
| --- | --- |
| 数据集 | Iris Plants Database，UCI Machine Learning Repository dataset 53 |
| 页面 / 归档 | <https://archive.ics.uci.edu/dataset/53/iris> / `iris.zip` |
| DOI / 许可 | `10.24432/C56C76` / CC BY 4.0 |
| 原始文献 | Fisher, R.A. (1936), *The use of multiple measurements in taxonomic problems* |
| 获取日期 | 2026-10-18 |
| 归档 SHA256 | `d11fe30213d36434a0879aab7cb00ce3c812eb7ba2495874438abff7b7b762e9` |
| 规模 | 150 条记录，4 个数值特征，3 个类别各 50 条 |
| `iris.data` SHA256 | `6f608b71a7317216319b4d27b4d9bc84e6abd734eda7872b71a458569e2656c0` |

**案例使用真实公开数据，明确不是合成数据。** 来源元数据和逐文件哈希见 `examples/research/iris/iris.json`；数据说明、归档清单和另一整理版本也一并保留。

## 2. 多步骤分析流程与人工依赖核对

`analyze_iris.py` 仅使用 Python 标准库，执行五步：

1. 读取 `analysis-config.json`，校验特征顺序、5 列结构、预期 150 行和三类各 50 条；
2. 读取 `iris.data`，逐行校验数值有限性和类别合法性；
3. 按类别分组，计算 4 个特征的组内均值与总体均值（保留 6 位小数）；
4. 读取 `iris-baseline.json`，与人工复算的基线逐项核对；
5. 写出排序稳定的 `result.json`；校验失败写 `failure.json` 并以退出码 2 结束。

| 文件 | 分析程序是否读取 | 角色 |
| --- | --- | --- |
| `analyze_iris.py` | 是 | 分析代码 |
| `iris.data` | 是 | 唯一样本数据输入 |
| `analysis-config.json` | 是 | 特征、规模与舍入配置 |
| `iris-baseline.json` | 是 | 独立复算基线 |
| `bezdekIris.data` | **否** | 未使用文件对照（同一归档的另一整理版本） |
| `iris.names`、`Index` | **否** | 数据集说明与归档清单 |
| `iris.json`、`README.md` | **否** | 来源元数据与案例说明 |

真实挂载验收断言实际正文输入恰好是前四个文件，`bezdekIris.data`、`iris.names`、`Index` 等不出现在 `capture.inputs` 中。

## 3. 新增的通用报告能力

- **输出摘要**：报告收尾时递归记录 `outputs/` 下全部普通文件及顶层 `stdout.txt`、`stderr.txt` 的路径、字节数和 SHA256；无法读取时 `complete=false`，verify 拒绝。
- **轻量环境摘要**：`environment` 记录 OS、内核、机器、主机、uid/gid、Python、CPU 数、内存、WSL 标志和 TeamFS 版本，并附 `disclaimer` 声明它不是完整环境归档。
- **报告比较**：`teamfs experiment compare <报告A> <报告B> [--json]` 按原始路径字节配对输入和输出，列出新增、删除、内容变化、访问分类变化和未变；实验名、PID、时间戳、读取计数和退出状态只在 `run_context` 中单列。
- **verify 扩展**：在输入 SHA256 之外，重新计算报告目录内每个输出文件的 SHA256/字节数，并拒绝含 `..` 的输出路径。

## 4. 最近一次端到端实测（2026-10-18）

运行环境：Ubuntu 24.04.1 LTS（WSL2），内核 `6.6.114.1-microsoft-standard-WSL2`，Python 3.12.3，TeamFS 0.8.0，debug 构建。

复现命令：

```bash
bash scripts/member4-case.sh
```

时间戳目录：`artifacts/member4-case/20261018-105024-teamfs-member4-case-26vbbh9u/`，最新位置见 `artifacts/member4-case-latest.json`。

### 4.1 原始输入运行

实际正文输入 4 个：`analyze_iris.py`、`iris.data`、`analysis-config.json`、`iris-baseline.json`。输出摘要 3 个：`outputs/result.json`、`stdout.txt`、`stderr.txt`。`bezdekIris.data` 未被读取。

- `result.json` SHA256：`ef98b8d7ea211952c020d61aa2c419c19ae464c81ffd269e0f2921a0315bab62`
- setosa 萼片长均值：5.006；`baseline_check.matched=true`
- 轻量环境摘要包含 `os_release=Ubuntu 24.04.1 LTS`、`python3=Python 3.12.3`、`cpu_count=16`、`teamfs_version=0.8.0`、`wsl=true`

### 4.2 修改同名数据后运行

把 `iris.data` 首行的萼片长 5.1 改为 5.2，文件名不变。分析仍成功，但：

- 输入 `iris.data` SHA256 改变；
- setosa 萼片长均值从 5.006 变为 5.008；
- `baseline_check.matched=false`，明确记录与原始基线的差异；
- `result.json` SHA256 变为 `10bd957f7203d814a7dfe27dbbf6c54c27e0e5ea761568030aa080bbfab00a33`。

`experiment compare` 结果（`artifacts/member4-compare-changed.json`）：

| 维度 | 结果 |
| --- | --- |
| 输入 | 新增 0，删除 0，内容变化 **1**（`iris.data`），访问分类变化 0，未变 3 |
| 输出 | 新增 0，删除 0，内容变化 **3**（`outputs/result.json`、`stdout.txt`、`stderr.txt`），未变 0 |
| 结论 | 输入内容一致=否，输出一致=否，两份报告均采集完整 |

验收套件另验证同名配置变化：把 `rounding_decimals` 从 6 改为 2，setosa 萼片长均值输出为 5.01，比较正确列出 `analysis-config.json` 为内容变化。

### 4.3 旧输入重放

```bash
teamfs experiment replay "$MOUNT" <baseline-report> iris-replay --output <replay-report>
```

- 重放 `result.json` 与基线**逐字节一致**；
- 两者 SHA256 均为 `ef98b8d7ea211952c020d61aa2c419c19ae464c81ffd269e0f2921a0315bab62`；
- `stdout.txt`、`stderr.txt` 也逐字节一致；
- `experiment compare` 显示输入新增/删除/内容变化/分类变化均为 0（4 个未变），输出变化为 0（3 个未变），`inputs_content_identical=true`、`outputs_identical=true`；
- `experiment verify` 对重放报告重新校验输入和输出均通过。

重放比较结果保存在 `artifacts/member4-compare-replay.json`。

### 4.4 失败结果记录

把首行特征改成非数值 `oops` 后运行：

- 程序退出码 2，报告 `outcome=program_failed`；
- 仍记录输入 SHA256、退出状态、环境摘要和 3 个输出摘要；
- 生成 `outputs/failure.json`，错误信息为“第 1 行包含非数值特征”；
- 与基线比较：输出新增 `outputs/failure.json`、删除 `outputs/result.json`，输入 `iris.data` 为内容变化；
- `experiment verify` 仍能校验这份失败运行的文件证据。

### 4.5 输出篡改与越界路径拒绝

验收套件复制基线报告后：

- 改写 `outputs/result.json` 正文，verify 报“输出校验不一致”；
- 把输出清单路径改为 `../escape`，verify 以“输出路径”错误拒绝。

## 5. 证据索引

| 产物 | 内容 |
| --- | --- |
| `artifacts/member4-acceptance.json/.txt` | 6 项真实挂载验收（全部通过） |
| `artifacts/member4-case-latest.json` | 最新案例目录与完整摘要 |
| `artifacts/member4-case/<时间戳>/summary.json` | 数据来源、检查项、三份报告与结果 |
| `.../baseline/`、`.../changed/`、`.../replay/` | 保留的完整实验报告 |
| `.../backup/` | 修改数据前导出的独立输入备份（已重新校验） |
| `.../compare-changed.json`、`.../compare-replay.json` | 两次报告比较的完整 JSON |
| `artifacts/member4-compare-changed.json`、`artifacts/member4-compare-replay.json` | 固定位置的比较结果 |
| `artifacts/member4-case.txt` | 案例运行的文本输出 |
| `artifacts/member4-case-trace.log` | 真实 FUSE 回调日志（本地产物；仓库 `.gitignore` 忽略 `artifacts/*.log`，不随提交分发） |
| `examples/research/iris/` | 数据、来源元数据、分析程序与人工核对说明 |

## 6. 可复现步骤

```bash
# 在项目目录下的 Linux / WSL2 中
bash scripts/test-member4.sh     # 验收：输入集合、输出/环境证据、比较、重放、失败与篡改
bash scripts/member4-case.sh     # 端到端案例，生成新的时间戳证据目录
bash scripts/check.sh            # 全套回归，member4 已纳入 regression-summary
```

移交时保留整个项目目录；`artifacts/member4-case/<时间戳>/baseline` 与 `backup` 可用于在新的 TeamFS 挂载中验证或重放旧输入。报告自身不包含输入正文，长期迁移需同时保留报告和独立 backup。

## 7. 可验证的结论与边界

**可以验证的结论：**

- 在本机真实 FUSE 挂载中，分析程序实际读取的公开 Iris 输入集合可被准确列出，未使用文件不会混入；
- 同名数据或配置改变会同时反映在输入 SHA256、输出 SHA256 和比较结果中；
- 同一环境、同一构建下，用旧输入重放得到逐字节一致的结果；
- 报告比较只把文件内容变化计入，实验名/PID/时间/读取计数变化不会被误报；
- 成功与失败运行都保留可校验的输入、输出、退出状态和环境摘要。

**边界：**

- 环境摘要是轻量解释上下文，不是完整环境归档，不含软件包清单、库版本、全部环境变量或硬件细节；
- 重放一致性只证明同机同环境的确定性案例，不声称任意环境下的科学复现；
- 公开数据可信不等于任意科研程序可信；追踪范围仍限于指定挂载和进程组；
- SHA256 用于一致性核对，不是对抗恶意修改的签名；报告与后端由本机用户管理；
- 案例只计算描述性统计均值，不据此提出新的植物学结论。

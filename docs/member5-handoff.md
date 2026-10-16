# 成员 5 性能评估对接说明

本轮交付当前 0.8 版本的性能基线、独立评估脚本、已保存结果的版本对比工具及对接约定。其他成员无需等待性能工作即可继续开发；成员 5 在功能合并后使用相同脚本复测。当前数据不是最终整合版本验收，也没有新增功能已完成或优化已生效的含义。

## 已交付内容

| 内容 | 入口 | 当前状态 |
|---|---|---|
| 三组真实对照 | `scripts/benchmark-member5.sh` | release 模式，原生目录、普通 TeamFS、实验模式 |
| 合成负载和采样 | `tests/member5_benchmark.py`、`tests/member5_workload.py` | 小文件、多文件、大文件；隔离挂载与数据库 |
| 可复查证据 | `artifacts/member5/20261016T034314Z-ipiu8x6_/` | 27 个样本全部通过；9 次实际 experiment verify |
| 图表 | `scripts/plot-member5.py` | 从已保存样本生成 PNG/SVG，柱为中位数、点为全部样本 |
| 两次评估对比 | `scripts/compare-member5.py` | 核对条件，输出中位数变化、百分比和缺失项 |
| 证据归档 | `scripts/package-member5.py` | 原字节 ZIP、逐文件哈希及归档回读验证 |
| 方法及结果解释 | `docs/member5-performance.md`、`docs/member5-baseline-results.md` | 计时/内存/存储口径和现阶段结论 |

本轮没有修改 `src/`、原有共享测试工具、命令协议及实验报告格式。没有加入周期保存、输出摘要、真实案例或报告页面。工作负载是合成顺序读取加 SHA256，不能称作纯磁盘吞吐测试。

Git 中的历史样本子目录和 source 快照收录在 `raw-evidence.zip`；顶层汇总、fixture 和图表可以直接浏览，完整证据解压到新目录后核对。`evidence-archive.json` 给出归档及逐文件 SHA256。

## 各成员需要对接的内容

| 成员 | 请提供或确认 | 成员 5 如何使用 | 完成后如何验收 |
|---|---|---|---|
| 2 访问与联调 | experiment CLI、访问分类和字段变更；整合提交号及回归结果 | 更新适配，确保性能样本没有漏采或错采 | 原有正确性套件通过，基准内的输入/字节数检查通过 |
| 3 持久保存 | 中途保存开关、默认间隔、保存次数及保存耗时的口径 | 比较同一提交下关闭/开启保存及不同间隔 | 分别记录成功保存与不完整状态，性能样本不掩盖功能失败 |
| 4 案例与报告 | 公开数据来源、下载/准备方式、启动命令、输入规模、输出路径和预期结果；输出摘要接口 | 增加真实案例负载，测量输出摘要及报告比较成本 | 三组业务输出一致；重放满足确定性要求；外部依赖单独说明 |
| 6 页面与发布 | 最终发布提交号、报告消费格式、展示需要的图表字段 | 提供最终数据和对应图表，供页面/PPT 使用 | 页面与图表标明版本、构建、单位、样本数和测量范围 |

请在改动命令参数、manifest 字段、快照路径或访问分类前同步成员 5。新增字段优先采用向后兼容方式；现有字段若要删除或改语义，应先约定迁移方式。表中需求是待双方确认的对接事项，不表示组员已承诺实现。

## 当前脚本依赖的现有接口

脚本使用以下既有命令：

```text
teamfs mount <mount> --store <store> --no-audit-file --capacity-mib 128 --max-file-mib 32
teamfs sync <mount>
teamfs experiment run <mount> bench --output <new-report-dir> -- <python> -B workload.py <count> <rounds>
teamfs experiment verify <mount> <report-dir>
```

实验运行命令必须将程序工作目录设为固定快照，并提供 `TEAMFS_OUTPUT` 指向挂载点外的 `outputs/`。报告目录需包含 `manifest.json`。目前读取的核心字段为：

- 顶层 `phase`、`capture_complete`、`exit_code`、`cancelled`。
- `capture.inputs[]` 的 `path_bytes`、`sha256`、`access_class`、`read_bytes`。
- `capture.unpinned_mount_accesses`、`capture.external_link_accesses`。
- `.teamfs/status.json`、`.teamfs/metrics.json` 仅归档，当前不靠其内部指标推算命令分阶段时间。

新增阶段计时字段之前，脚本输出的 `snapshot_ms`、`capture_hash_ms`、`report_export_ms` 均为 null；这表示未测量，不能解读为零开销。`verify_cli_ms` 是命令结束之后另行校验的时间，不是运行结束时的输入哈希耗时。

## 阶段计时接口提案

以下是待成员 2/4 确认的提案，本轮尚未改动 Rust。建议由实验运行模块维护者增加可选的计时对象，用 `std::time::Instant` 计算耗时，以整数微秒表示，报告中的 Unix 时间戳继续只用于定位时间。

| 建议字段 | 起止范围 | 包含关系 |
|---|---|---|
| `timing_us.input_pin` | runner 发送 ExperimentStart 到成功响应 | 包含命令传递和快照事务提交，不等于纯数据库时间 |
| `timing_us.command_wait` | 释放执行门到发现主进程退出 | 包含调度和当前退出轮询；不能直接称作程序 CPU 时间 |
| `timing_us.capture_finalize` | runner 发送 ExperimentFinish 到收到采集结果 | 包含汇总、输入哈希及命令传递 |
| `timing_us.input_hash` | daemon 在最终采集中计算输入 SHA256 的区间 | 属于 capture_finalize 的子阶段，不能重复相加 |
| 报告写入外部测量 | prepared/running/final 报告持久保存的各次调用 | 最终保存的时间在保存后才可知，宜写入独立计时文件或结构化 stdout |

不要为了把“最终报告保存耗时”塞进同一个 manifest 再无条件重写一次报告，这会改变被测开销，也产生定义上的循环。若采用独立 `timings.json`，应明确该计时文件自身的落盘是否包含在命令总耗时中。

成员 3 的周期保存建议另给出保存次数、成功/失败次数及累计耗时，注明它们是运行阶段的子集；不要把其累计时间与完整运行时间再相加。先以真实测量为准，不要求承诺固定百分比开销。

## 复测与版本对比

建议始终保留同一测量脚本，只更新目标 TeamFS 代码。若工作负载或计时脚本确实需要修改，应尽可能用更新后的同一脚本对旧、新两版分别重测；否则比较工具会标记测量条件不同。

在 Linux/WSL 项目目录中：

```bash
# 先确保本分支工作已提交，检查工作区，再合并团队更新；不要盲目覆盖本地修改
git status
git fetch origin
git merge origin/main

# 每次自动新建结果目录；构建结束后才进入计时
bash scripts/benchmark-member5.sh --samples 3

# 对比两个已有正式评估目录，输出目录必须不存在
python3 scripts/compare-member5.py \
  artifacts/member5/<before-run> artifacts/member5/<after-run> \
  --output artifacts/member5-comparisons/<comparison-name>
```

工具要求两边套件成功、每组至少 3 次、分组完整且无重复、输入 fixture 和确定性输出一致。失败样本、输入变化、结果变化不能通过 `--allow-mismatch` 绕过。

默认还核对 profile、Python/Rust/FUSE、CPU/内核、工作文件系统、模式顺序、测量说明和测量源码哈希。`--allow-mismatch` 仅允许对环境或测量方法差异生成明确标注为探索性的报告，不能当作控制变量充分的优化证明。记录一致也不证明后台负载、电源状态和缓存完全相同，仍需结合原始散点和重复测量分析。

比较结果中百分比为 `(新中位数 / 旧中位数 - 1) × 100%`。正值表示该指标增大；基线为 0 或缺失的阶段显示 N/A。程序内部 body 已包含在 command wall 中，禁止把二者相加。工具不自动用某个阈值判定项目失败。

## 合并和发布检查

本次使用独立分支提交，通过 Pull Request 合并，不直接覆盖主分支。评审重点是测量口径、正确性核对、故障证据和可复跑性，而不是要求追溯模式快于原生目录。

成员 5 的工具检查：

```bash
python3 -m unittest discover -s tests -p 'test_member5_compare.py' -v
bash scripts/benchmark-member5.sh --samples 1 --workloads small
```

工具测试包含环境/构建变化、错误输入输出、样本不足、重复样本、缺失/非有限指标及零基线等情形。冒烟样本只验证运行链路，不用于正式对比。

最终整合时仍须在目标提交重新运行完整性能基线、实际科研案例以及项目全套 `scripts/check.sh`。发布材料应同时给出目标提交号、测量源码哈希、release/debug、缓存和日志条件、机器、原始样本及阶段边界。不得将本轮 25 项单元测试与 27 个基准样本通过写成全套回归通过。

当前仍待完成：内部阶段接口接入、周期保存成本、真实科研案例、基于证据的优化前后对照，以及最终整合版本验收。

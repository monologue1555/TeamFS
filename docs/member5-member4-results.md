# 成员 4 合入后的性能评估

本轮接入主分支 `c856f3f`：成员 4 的输出摘要、轻量环境信息、报告比较与公开 UCI Iris 案例已经可用。成员 5 分支保留开放 PR，暂不合并主分支。

本轮分为两种测量：使用相同合成负载比较 `9e4ced3` 与 `c856f3f`；在 `c856f3f` 上运行成员 4 原样的 Iris 程序，比较原生目录、普通 TeamFS 与实验模式，并独立测量 verify、重放和报告比较。二者分别报告，不混用基线。

## 实测结果

108 个合成样本、15 个 Iris 样本全部通过；另有 5 次改动当前数据后的旧输入重放成功。合成评估 36 次 verify，Iris 原实验和重放 10 次 verify，合计 46 次，均通过；Iris 的 5 次 compare 均确认输入、输出内容一致。旧、新版 release Rust 单元测试分别为 30/32 项通过；成员 5 的 22 项工具测试通过。

| Iris 模式/命令 | 样本数 | 耗时中位数 ms |
|---|---:|---:|
| 原生目录运行 | 5 | 22.720 |
| 普通 TeamFS 运行 | 5 | 27.199 |
| 实验模式运行 | 5 | 89.395 |
| 实验后的 verify | 5 | 6.176 |
| 改动当前数据后的旧输入重放 | 5 | 89.677 |
| 原实验与重放报告 compare | 5 | 1.234 |

Iris 实验报告顶层元数据中位数为 13,033 字节，业务结果另算。实验模式比普通模式多出的时间包含快照、采集、输入/输出校验、环境信息和报告发布等工作，不能单独归因于某一步。

合成对照中，small 实验模式由 111.292 ms 变为 118.058 ms（+6.08%），但原生与普通模式同时上涨约 21.06%/16.48%；large 实验模式仅变化 +0.44%。这说明要保留全部模式与散点，不宜从一次整版顺序测量推导固定的输出摘要开销比例。两版的 Python/Rust/FUSE、测量脚本、输入与输出等记录条件匹配；仍没有严格控制缓存和后台活动。

- [合成版本对照及图表](../artifacts/member5-studies/20261018-member4-v1/report.md)
- [合成负载的 daemon 阶段](../artifacts/member5-studies/20261018-member4-v1/daemon-stages/daemon-stages.md)
- [公开 Iris 全部样本、图表与证据](../artifacts/member5-studies/20261018-member4-iris/report.md)
- [原始证据核验](../artifacts/member5-studies/20261018-member4-v1/validation.json)：3 个归档共 1262 个文件逐项哈希一致；28 份新版报告内 102 个输出文件摘要与实际字节一致。

本机已在独立、初始干净的 `c856f3f` 检出目录运行完整 `bash scripts/check.sh`：**177 项通过、0 失败、8 项未支持、0 环境跳过**，退出码 0。开启 `TEAMFS_REQUIRE_TOOLS=1`；测试包含成员 3/4 套件，采用 debug 构建，性能测量另用 release。未支持项包括硬链接、xattr、相关 mmap 边界及未显式 sync 的 SQLite DELETE 持久性场景，不计入通过。见 [完整回归记录](../artifacts/member5-studies/20261018-member4-regression/regression-summary.md)；日志和顶层回归产物 ZIP 保留在同目录。此结论针对当前固定提交，后续整合仍需复测最终提交。

合成运行的测量脚本沿用上一轮，原始 summary 中含针对成员 3 的历史说明；本轮目标提交明确为 `9e4ced3 → c856f3f`，差异是成员 4 的整组功能。该原始说明保留供审计，本报告按实际目标提交解释，不改写已归档证据。

## 公开案例的正确性

- 使用公开 UCI Iris 原始数据，150 行、4 个特征、3 个类别，数据文件 4551 字节。来源 DOI：10.24432/C56C76，许可 CC BY 4.0。
- 成员 4 分析程序保持原样。成员 5 用独立 Decimal 算术复算组内和总体均值，核对行数、类别数量、输入大小与 SHA256。
- 三种模式要求业务结果逐字节一致。实验输入清单必须恰好包含分析程序、数据、配置与基线文件；另一份数据和文档不应被误记为实际读取。
- 输出清单中的结果文件、stdout、stderr 均按实际字节数与 SHA256 核对，要求报告完整。
- 每次实验后修改当前同名数据，再重放原报告；结果必须仍与原结果逐字节一致。原实验及重放分别执行 verify，compare 必须确认输入和输出内容一致。

Windows 的默认 Git 换行转换曾把 `iris.data` 从 4551 字节变成 4702 字节，导致公开数据哈希不匹配。本轮为 `examples/research/iris/*` 加入原字节保留规则，并从原 Git 对象恢复文件，文件内容与成员 4 提交的 Git 对象完全一致。后续跨平台检出也应保留同样的输入身份。

## 复跑

先按 [成对评估方法](member5-performance.md#两个提交的同日复测) 构建两个固定提交，保存构建与单元测试日志，再运行同一套合成评估。公开案例需要使用其中 `c856f3f` 对应的 release 二进制：

```bash
python3 tests/member5_iris.py --binary <c856f3f-release-binary> \
  --target-ref c856f3f --samples 5 --output artifacts/member5/<新的Iris目录>
python3 scripts/package-member5.py artifacts/member5/<新的Iris目录>
python3 scripts/report-member5-iris.py artifacts/member5/<新的Iris目录> \
  --output artifacts/member5-studies/<新的Iris报告目录>
```

输入准备、挂载和归档不计入命令耗时。重放、verify、compare 都是额外命令，不能作为原实验命令的内部阶段相加。为保持成员 4 程序原样，没有新增 body 或进程 RSS 插桩；相关指标为 N/A。

Iris 很小，主要用于观察启动及追溯固定成本，不代表大数据吞吐。合成版本对照包含输出摘要、环境采集及 verify 行为变化，不能把差值全部解释为输出哈希耗时，也不是输出摘要开关对照。

## 后续对接

成员 4 的真实案例与输出摘要接口已接通，不再列为待交付。后续仍需周期保存开关与间隔、runner 端到端计时、纯输入/输出哈希与外部报告写入计时；优化要另做同条件前后对照。成员 6 可以引用明确版本的阶段结果，最终发布仍应复测最终整合提交。

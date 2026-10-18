# 成员 5 性能证据

10 月 18 日成员 3 合入后的两轮证据以 `20261018-member3-v1-*` 和 `20261018-member3-reverse-*` 命名。四个目录各有 54 个样本，完整原始数据收录在各自 ZIP 中；[汇总报告](../member5-studies/20261018-member3-combined/report.md)保留两轮全部数据，不能只选择其中一轮。历史基线如下。

正式基线目录：[`20261016T034314Z-ipiu8x6_`](20261016T034314Z-ipiu8x6_/report.md)。

该目录包含 27 个真实运行样本、9 组各 3 次对照；目标 Rust 代码来自提交 `276e68e37381a2c56c853ca7243859c340431213`。测量时评估工具是未提交的新增文件，因此同时保留 `source/` 快照和 SHA256，不能把目标提交号误当作已包含评估脚本。

- `summary.json`、`samples.csv`：完整机器可读样本和环境。
- `fixture-*.json`：确定性输入版本和预期输出。
- 各样本子目录：实际命令、输出、实验报告、输入校验结果和运行指标。
- `source/`：当次测量的源文件副本，只用于历史审计；日常修改仓库根目录下的正式源文件。
- `timing-comparison.png/.svg`：中位数柱状图和全部样本散点。
- `chart-source.json`：图表输入与绘图脚本哈希。
- `validation.json`：当次证据完整性核对，未执行全套回归。

为减少重复源码和大量生成内容对代码评审的干扰，Git 中保留顶层汇总、fixture、图表及 `raw-evidence.zip`。所有样本子目录和 `source/` 原样收录在 ZIP 中；`evidence-archive.json` 记录 ZIP 和每个文件的 SHA256。可用 `python3 -m zipfile -e <raw-evidence.zip> <新建目录>` 解压审计。归档包含顶层元数据，可直接作为完整的历史评估目录使用。

历史样本和 source 快照应保持原样，新增测量使用新目录。报告中的临时 Linux 路径属于运行位置记录，相关临时挂载和数据库在成功后已清理；复现时使用脚本重建输入，不依赖旧绝对路径。

中文解读见 [首轮结果](../../docs/member5-baseline-results.md)，对接与复测见 [对接说明](../../docs/member5-handoff.md)。

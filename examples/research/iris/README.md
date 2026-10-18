# UCI Iris 真实公开数据案例

本目录是成员 4 的科研案例：使用来源明确的公开数据和多步骤分析流程，演示输入依赖、
同名数据变化和旧版本重放。**这里的 `iris.data` 是真实公开数据，不是合成数据。**

## 数据来源与规模

- 数据集：Iris Plants Database，UCI Machine Learning Repository dataset 53。
- 数据集页面：<https://archive.ics.uci.edu/dataset/53/iris>
- 归档地址：<https://archive.ics.uci.edu/static/public/53/iris.zip>
- DOI：10.24432/C56C76；许可：CC BY 4.0。
- 原始文献：Fisher, R.A. (1936). *The use of multiple measurements in taxonomic
  problems*. Annals of Eugenics, 7(2), 179-188。
- 获取日期：2026-10-18；归档 SHA256：`d11fe30213d36434a0879aab7cb00ce3c812eb7ba2495874438abff7b7b762e9`。
- 规模：150 条记录，4 个数值特征（萼片长/宽、花瓣长/宽，单位 cm），
  3 个类别（Iris-setosa、Iris-versicolor、Iris-virginica），每类 50 条；`iris.data` 共 4551 字节。

文件 SHA256：

| 文件 | SHA256 |
| --- | --- |
| `iris.data` | `6f608b71a7317216319b4d27b4d9bc84e6abd734eda7872b71a458569e2656c0` |
| `bezdekIris.data` | `0fed2a99db77ec533a62dc66894d3ec6df3b58b6a8f3cf4a6b47e4086b7f97dc` |
| `iris.names` | `71a09fb3ee237614cdb9c09d06e8ae1f610ce2a274c0d77272fea8a233e5eea3` |
| `Index` | `fc3a4464fd2138ca649be5e0311270f5cf1f045c8e3ff1cd458f663dfe4f6790` |

完整来源元数据见 `iris.json`。

## 多步骤分析流程

`analyze_iris.py` 只使用 Python 标准库，按以下顺序执行：

1. 读取 `analysis-config.json`，校验特征顺序、列数、预期行数和类别分布；
2. 读取 `iris.data`，逐行校验 5 列、4 个有限数值和已知类别；
3. 按类别分组，计算每类 4 个特征的均值，并计算总体均值（保留 6 位小数）；
4. 读取 `iris-baseline.json`，把计算结果与独立基线逐项核对；
5. 将确定性结果写入 `$TEAMFS_OUTPUT/result.json`。

输入校验失败时写入 `$TEAMFS_OUTPUT/failure.json` 并以退出码 2 结束；基线不一致不会
阻止输出，但会在 `result.json` 中标记 `baseline_check.matched=false` 并列出差异。

## 文件依赖人工核对

| 文件 | 是否被分析程序读取 | 说明 |
| --- | --- | --- |
| `analyze_iris.py` | 是（解释器读取代码正文） | 分析程序本身 |
| `iris.data` | 是 | 唯一样本数据输入 |
| `analysis-config.json` | 是 | 特征、规模、舍入和基线容差配置 |
| `iris-baseline.json` | 是 | 独立复算的三类均值，用于自检 |
| `bezdekIris.data` | **否** | 同一归档中的另一整理版本，作为未使用文件对照 |
| `iris.names` | **否** | 数据集说明文档 |
| `Index` | **否** | 归档文件清单 |
| `iris.json` | **否** | 来源元数据 |
| `README.md` | **否** | 本说明 |

`bezdekIris.data` 与 `iris.data` 字节不同（SHA256 不同），因此如果分析程序错误地把
整个目录当成输入，实验报告会立刻暴露；验收测试会断言它不出现在实际正文输入中。

## 本地运行（不经过 TeamFS）

```bash
cd examples/research/iris
TEAMFS_OUTPUT=/tmp/iris-out python3 -I analyze_iris.py \
    iris.data analysis-config.json iris-baseline.json
```

预期终端输出（节选）与 `result.json` 中的三类均值：

| 类别 | 萼片长 | 萼片宽 | 花瓣长 | 花瓣宽 |
| --- | --- | --- | --- | --- |
| Iris-setosa | 5.006 | 3.418 | 1.464 | 0.244 |
| Iris-versicolor | 5.936 | 2.77 | 4.26 | 1.326 |
| Iris-virginica | 6.588 | 2.974 | 5.552 | 2.026 |

总体均值为萼片长 5.843333、萼片宽 3.054、花瓣长 3.758667、花瓣宽 1.198667。
`iris-baseline.json` 与上述组内均值一致，分析程序会在 `result.json` 中记录
`baseline_check.matched=true`。

## 确定性说明

- 结果按固定键排序输出，不使用随机数、当前时间、主机名或输出目录路径；
- 输入路径按命令行传入的相对名称记录，重放时保持相同相对名称即可逐字节一致；
- `result.json` 记录三份输入的 SHA256，因此同名文件内容变化会同时改变输入与输出摘要。

## 边界

- 本案例只演示确定性统计流程，不对植物学分类提出新结论；
- 数据集本身公开可信，但案例不能证明任意科研程序都能完全复现；
- TeamFS 的实验报告记录轻量环境摘要，不是完整环境归档；
- 数据库说明和未使用文件只用于人工核对，不参与计算。

TeamFS 端的运行、报告比较与旧版本重放证据见项目根目录
[`docs/member4-case.md`](../../../docs/member4-case.md) 和 `artifacts/` 下的验收产物。

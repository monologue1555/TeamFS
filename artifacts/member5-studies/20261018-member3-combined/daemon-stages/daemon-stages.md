# 现有 daemon 指标中的子阶段耗时

以下是原始 management 指标的提取，中位数单位 ms，所有样本均保留。

不同运行批次分别统计，不跨环境合并。

| 负载 | 运行批次 | 目标提交 | n | 启动处理 | 其中事务提交 | 摘要次数范围 | 摘要累计处理 | 每次摘要平均处理 | 最终采集处理 |
|---|---|---|---:|---:|---:|---|---:|---:|---:|
| small | 20261018-member3-v1-before | 276e68e | 3 | 5.768 | 5.737 | N/A | N/A | N/A | 0.071 |
| many | 20261018-member3-v1-before | 276e68e | 3 | 6.312 | 6.099 | N/A | N/A | N/A | 5.111 |
| large | 20261018-member3-v1-before | 276e68e | 3 | 11.534 | 11.506 | N/A | N/A | N/A | 13.231 |
| paced_many | 20261018-member3-v1-before | 276e68e | 3 | 6.098 | 5.837 | N/A | N/A | N/A | 5.217 |
| paced_large | 20261018-member3-v1-before | 276e68e | 3 | 13.229 | 13.197 | N/A | N/A | N/A | 14.215 |
| unused_large | 20261018-member3-v1-before | 276e68e | 3 | 20.520 | 20.489 | N/A | N/A | N/A | 0.068 |
| small | 20261018-member3-v1-after | 9e4ced3 | 3 | 7.437 | 7.404 | 1–1 | 0.159 | 0.159 | 0.062 |
| many | 20261018-member3-v1-after | 9e4ced3 | 3 | 12.497 | 12.278 | 1–1 | 3.280 | 3.280 | 4.931 |
| large | 20261018-member3-v1-after | 9e4ced3 | 3 | 13.200 | 13.161 | 1–1 | 16.800 | 16.800 | 14.504 |
| paced_many | 20261018-member3-v1-after | 9e4ced3 | 3 | 6.883 | 6.492 | 6–6 | 30.141 | 5.024 | 5.598 |
| paced_large | 20261018-member3-v1-after | 9e4ced3 | 3 | 11.470 | 11.440 | 5–5 | 70.438 | 14.088 | 15.637 |
| unused_large | 20261018-member3-v1-after | 9e4ced3 | 3 | 9.467 | 9.432 | 1–1 | 13.504 | 13.504 | 0.071 |
| small | 20261018-member3-reverse-before | 276e68e | 3 | 4.910 | 4.887 | N/A | N/A | N/A | 0.060 |
| many | 20261018-member3-reverse-before | 276e68e | 3 | 6.388 | 6.186 | N/A | N/A | N/A | 5.730 |
| large | 20261018-member3-reverse-before | 276e68e | 3 | 11.002 | 10.974 | N/A | N/A | N/A | 12.670 |
| paced_many | 20261018-member3-reverse-before | 276e68e | 3 | 6.929 | 6.691 | N/A | N/A | N/A | 6.611 |
| paced_large | 20261018-member3-reverse-before | 276e68e | 3 | 12.350 | 12.308 | N/A | N/A | N/A | 16.206 |
| unused_large | 20261018-member3-reverse-before | 276e68e | 3 | 11.715 | 11.672 | N/A | N/A | N/A | 0.065 |
| small | 20261018-member3-reverse-after | 9e4ced3 | 3 | 6.447 | 6.425 | 1–1 | 0.159 | 0.159 | 0.087 |
| many | 20261018-member3-reverse-after | 9e4ced3 | 3 | 7.993 | 7.748 | 1–1 | 2.718 | 2.718 | 5.917 |
| large | 20261018-member3-reverse-after | 9e4ced3 | 3 | 11.679 | 11.646 | 1–1 | 15.528 | 15.528 | 13.077 |
| paced_many | 20261018-member3-reverse-after | 9e4ced3 | 3 | 7.833 | 7.610 | 6–6 | 32.177 | 5.363 | 6.885 |
| paced_large | 20261018-member3-reverse-after | 9e4ced3 | 3 | 12.408 | 12.362 | 5–5 | 77.958 | 15.592 | 13.565 |
| unused_large | 20261018-member3-reverse-after | 9e4ced3 | 3 | 9.862 | 9.841 | 1–1 | 13.454 | 13.454 | 0.059 |

边界：启动事务是启动处理的子集；摘要累计包含初始摘要与周期摘要，不包括运行器向外部报告目录写文件。最终采集处理包含输入哈希与序列化，但没有单独拆出哈希时间。

这些数值不是互相独立的端到端阶段，不应再加到 command wall/body 上。测量位置见 src/service.rs 的 management 指标记录；已有指标不包含获取服务锁前的等待和内核请求传递。

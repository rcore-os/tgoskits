# Task-3 证据归档

> 分支 `openrace/task3-ai-control`；设计文档见 `book/design/task3-ai-design.md`。

## 指标与对比（M4）

| 文件 | 内容 |
|---|---|
| `run-{1..6}.csv` | 6 组原始日志 CSV（run-1..3 = AI，run-4..6 = baseline） |
| `ai-{1..3}.csv` / `baseline-{1..3}.csv` | 按模式整理的同数据 |
| `summary.csv` | 逐组 RMSE/调节时间/超调/RTT 汇总 |
| `comparison.png` | AI vs baseline 响应曲线对比图 |

核心结论：整体 RMSE AI 29.2–29.3 vs baseline 190.6–191.3；t500 稳态误差
~2 vs ~192；Guest 推理 11.3 ms 均值 / 14.6 ms p95。

## 故障闭环（M5）

| 文件 | 内容 |
|---|---|
| `fault-final3-*.log` / `fault-final4-*.log` | guest + proxy 日志（黑障→Safe→恢复→续跑） |
| `fault-final3-*pcap` / `fault-final4-*pcap` | 双端数据面抓包 |
| `fault-final3.sha256` / `fault-final4.sha256` | 上述证据哈希 |

关键事件：黑障 25s→35s（代理丢 102 帧）→ 双方 Safe → 恢复 → 控制环续跑
（final4 恢复后 82 个 STATUS 周期、0 错误）。

## 模型（M3）

模型结构/权重哈希/训练器哈希见
`components/task3-model/model/model.json`（13,089 参数，~0.7M MACs，
`weights.bin` SHA-256 内联记录）；golden-vector / golden-window 测试随
`cargo test -p task3-model` 可复算。

## 构建与运行命令

见 `book/design/task3-ai-design.md` §8。

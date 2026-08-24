# results/task1/virq-ab — 旧分支实验资产抢救与复算

> 抢救时间：2026-08-14，来源：实验 clone `/home/huhu/tgoskits-realtime`（分支
> `openrace/realtime-virq-ab`）的 `results/day4/ day5/ day6/` 工作区产物，
> 以及 git 分支 `realtime-virq-ab` 中 `docs/my/` 的两份私人文档。

## 状态总览

| 文件 | 来源 | 状态 | sha256 |
|---|---|---|---|
| `native.csv` | results/day4 | valid native QEMU reference | `e44592961dc0...b571` |
| `axvisor-zephyr-single.csv` | results/day4 | valid AxVisor single-Guest baseline | `7ef3a9baea81...e8f` |
| `axvisor-dual-idle.csv` | results/day5 | observational（无 Linux 证据） | `6800b4f1800a...a5` |
| `axvisor-dual-stress-unverified.csv` | results/day4 | 无效（无 stress marker） | `14cf644fe65a...07a` |
| `axvisor-dual-idle.log` | results/day5 | 原始日志（原名 `/tmp/day5-dual-idle-fixed.log`） | `7d5c179e7e38...70` |
| `axvisor-dual-stress.csv` | results/day5 | Linux SMP2 + stress marker 双证齐全 | `1a8f1c63968a...89c` |
| `axvisor-dual-stress.log` | results/day6 | 原始日志，CSV 仅 294 行（共用串口缺行），非正式 | `343bf96853c8...83` |
| `openrace-virq-ab-status.md` | git `realtime-virq-ab` `docs/my/` | 345 行，T4.1 素材 | 见 sha256sums |
| `openrace-realtime-progress.md` | git `realtime-virq-ab` `docs/my/` | 82 行，T4.1 素材 | 见 sha256sums |

完整 sha256：见 `sha256sums`（`sha256sum -c` 可验证）。

## 统计命令（可复算）

```bash
python3 scripts/test/rt_latency_stats.py results/task1/virq-ab/raw/native.csv
python3 scripts/test/rt_latency_stats.py results/task1/virq-ab/raw/axvisor-dual-idle.csv
python3 scripts/test/rt_latency_stats.py results/task1/virq-ab/raw/axvisor-dual-stress.csv
python3 scripts/test/virq_latency_stats.py results/task1/virq-ab/raw/axvisor-dual-idle.log   # 等 ab-* 原始日志补齐后使用
```

## 2026-08-14 复算结果（以本记录为准）

| 文件 | samples | mean(µs) | p99(µs) | p99.9/max(µs) | deadline_misses |
|---|---:|---:|---:|---:|---:|
| `native.csv` | 300 | 168.24 | 292.58 | 345.60 | 300 |
| `axvisor-zephyr-single.csv` | 300 | — | — | — | — |
| `axvisor-dual-idle.csv` | 300 | 257.26 | 524.86 | 932.27 | 300 |
| `axvisor-dual-stress.csv` | 300 | — | — | — | — |
| `axvisor-dual-stress.log`(294 行) | 294 | 165.04 | 299.81 | 533.71 | — |

### 数据完整性记录（已知问题）

1. **day5 README 数字与本次复算不一致**：旧 README 记录 dual-idle
   mean 253.909µs / p99 349.872µs / p99.9-max 1.344224ms，本次复算为
   mean 257.257µs / p99 524.864µs / p99.9-max 932.272µs。差异推测是 CSV 在旧
   README 统计后被重新生成覆盖。**结论：以本次复算为准，旧 README 数字不可复现，
   引用时标注。**
2. **virq-ab A/B 原始日志（`/tmp/ab-A*.log`、`ab-B*.log`、`e1-*.log`、
   `dual-boot.log`）本机不存在**：在实验机 `/tmp` 下，重启即丢。本次抢救仅覆盖
   day4-6 已入 clone 的产物。**mean 311→301µs、E1 虚假唤醒 124→0 的复算
   待原始日志补齐。**
3. **day6 stress CSV 只 294 行**：Linux 内核日志与 Zephyr CSV 共用串口丢行
   （缺 sequence 0,2,13,14,29,39），不用于正式 A/B 结论。

## 结论摘要（来自旧分支文档，待原始日志复算确认）

- vIRQ 队列 A/B：mean 311→301µs（噪声内，无统计显著差异）
- E1 虚假唤醒：124 → 0
- 已修 bug 清单见 `openrace-virq-ab-status.md`（5 个，含队列有界化、唤醒去重等）

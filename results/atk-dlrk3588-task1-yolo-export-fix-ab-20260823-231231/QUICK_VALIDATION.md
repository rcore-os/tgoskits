# Task 1 CSV 导出隔离：真实板快速 A/B 验证

日期：2026-08-23  
硬件：同一块 ATK-DLRK3588（RK3588）  
启动：RAM-only，未写 eMMC  
范围：RR 1 次 + FP-RR 1 次；这是因果修复验证，不替代正式 3+3 统计。

## 修改与验证问题

RT-Thread 在 18,000 个周期样本完成后先输出
`PERIODIC LATENCY SAMPLING COMPLETE`，随后阻塞等待 `d`。runner 在发送 `d` 前：

1. 收集 Host diagnostics；
2. 切回 AI Guest；
3. 要求 30 秒内观察到一次新的真实 ncnn/YOLO11n 推理完成；
4. 停止 AI；
5. 才切回 RT Guest 发送 `d` 并导出 CSV。

验证问题只有一个：旧 FP-RR 第 77 次 54～97 秒长尾是否由高优先级 RT CSV 导出污染，
而不是 FP-RR 采样阶段或 YOLO 本身造成。

## 快速结果

| 调度器 | RT 采样耗时 | 边界前 AI | 边界后 AI | 结果 |
|---|---:|---:|---:|---|
| RR | 778.715 s | sample 338：1.517 s | sample 339/340/341：1.523/1.534/1.536 s | AI 正常前进 |
| FP-RR | 187.077 s | sample 76：1.620 s | sample 77/78：1.608/1.586 s | 旧长尾消失 |

旧 FP-RR 三轮中，跨过导出起点的 sample 77 分别是 `79.425 s`、`54.202 s`、
`97.098 s`。新流程下 sample 77 为 `1.608 s`，且 RT 在此期间仍停在导出屏障。
因此快速验证支持以下因果结论：旧 AI 长尾来自高优先级、持续 runnable 的 CSV 导出，
不是 FP-RR 周期调度本身的固有代价，也不是死循环。

两臂均完成 18,000 行、最终 `PERIODIC LATENCY COMPLETE`，日志未发现 panic、
业务路径 ESR_EL2、fatal IRQ、output drop 或 runner error。诊断性的 jitter 结果仍显示
FP-RR 的实时收益（RR P99 `59.767 ms`，FP-RR P99 `0.577 ms`），但本文件不把单轮值
提升为正式统计结论。

## 本轮发现的独立探针边界

RR 本轮运行超过约 768.6 秒后，旧版 `cycles * 1_000_000_000` 的 64 位中间乘法溢出，
导致 sequence 17766～17999 的 timestamp/deadline 回绕。sequence 和 jitter 仍连续，
AI 边界验证不受影响，但该 RR CSV 不满足正式“时间戳严格递增”验收，因此不能进入
正式 3+3 RT 汇总。

源码已改为先计算商和余数再换算纳秒，分析器也已改为只从 CSV 表头后解析，相关测试
`36/36` 通过。修复后 18,000 样本 RT-Thread binary 已独立构建成功：

```text
/home/huhu/atk-bringup/task1-yolo-export-fix-ab-20260823-231231/night-inputs/rtthread/rtthread-periodic.bin
SHA256 7b863c7701d707a7e46cc68962e4eaa0e5b4882544c1be2168fc971160f12010
```

夜间 3+3 前必须用该 binary 重建 RR/FP-RR FIT，并先做一次时间戳跨 768.6 秒的 RR
验收；不得复用本轮 FIT。

## 原始证据

外部完整目录：

```text
/home/huhu/atk-bringup/task1-yolo-export-fix-ab-20260823-231231
```

本轮实际使用的 artifact 与日志 SHA256：

```text
RR FIT       1091904c7dffb6fa05030089d790830421daf11a4006938bbe66152f035334d5
FP-RR FIT    c7db4236accf6cd1455eabd9db0bab6394592c27eeebd987ef338ce0e6f85654
RT binary    ad4a5ae53de505d0daceb6997a42b3ae026df819927ecd8bb9f5495f7d1d1792
initrd       15db5f595a75dd3418d76252013ff6b5d27b7bb4c8c85889567231b9867ee452
RR log       de9e52c777daa5c17671e9d45252360d522d396f9e1cbd409daa50a90773a8f0
FP-RR log    5ab45690011cc5f9bee967b6d912169c0331127300f806e7c00399bd8286a4cc
```


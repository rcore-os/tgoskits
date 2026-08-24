# ATK-DLRK3588 同板多 RTOS 对比（统一证据更新）

> 2026-08-24 更新：旧版只包含 Zephyr 3.7.0 周期实验，并把 Task 2/3 标作未测。
> 该状态已经被同一个 Zephyr 4.4.2 Guest 的统一物理板实验取代。旧数字不再作为
> Zephyr 正式主链引用。

## 当前统一身份

| 项目 | RT-Thread | Zephyr |
|---|---|---|
| 物理板 | ATK-DLRK3588 | ATK-DLRK3588 |
| Hypervisor | AxVisor | AxVisor |
| RTOS 角色 | 高优先级控制 Guest | 高优先级控制 Guest |
| AI 角色 | StarryOS ncnn/YOLO | StarryOS ncnn/YOLO |
| 共享 CPU | pCPU `0x100` | pCPU `0x100` |
| Task 1 | RR/FP-RR 完成 | RR/FP-RR 完成 |
| Task 2 | IP/T2N1、pcap、故障恢复完成 | IP/T2N1、pcap、故障恢复完成 |
| Task 3 | manual/真实 YOLO 完成 | manual/真实 YOLO 完成 |

Zephyr 正式版本为 4.4.2，commit
`dccb09599635bdff17633fa7e9dab014b91dce90`，统一 Guest SHA256：

```text
ff316057f7fd829cdcd059f9fb7729bef0f877b8e2e486fbdb547bd3a6fe2522
```

RR 与 FP-RR 两个 FIT 均嵌入这一个 Guest，只切换 AxVisor 调度器，不是两个 Zephyr
版本，也不是两个 AxVisor 同时运行。

## Task 1 快速对比

| RTOS | 统计规模 | RR P99 | FP-RR P99 | P99 降幅 | RR/FP-RR max |
|---|---:|---:|---:|---:|---:|
| RT-Thread | 3+3，每轮 300 | 30.176 ms | 2.805 ms | 90.70% | 39.034 / 10.714 ms |
| Zephyr 4.4.2 | 1+1，每轮 300 | 6.420 ms | 0.507 ms | 92.10% | 10.395 / 2.490 ms |

不同 RTOS 的周期探针、内核和重复次数不同，因此只比较各自内部 RR→FP-RR 的改善，
不能用绝对 P99 宣称 Zephyr 天生优于 RT-Thread。

## Task 2/3 完成度

| 指标 | RT-Thread | Zephyr 4.4.2 |
|---|---:|---:|
| 双端 pcap | 83 帧/端，PASS | 24 帧/端，PASS |
| T2N1 账本 | 双端一致 | 双端一致 |
| 非法参数 ERROR 通知 | PASS | PASS，32 ms 收到 ERROR |
| blackout 重传/Safe/恢复 | 5 次，PASS | 5 次，PASS |
| manual 闭环 | 5/5 | 5/5 |
| YOLO 有效目标闭环 | 3/3 | 3/3 |
| 安全拒绝 | 2/2，100% | 2/2，100% |
| perception / target MAE | 0.00 / 26.33 | 0.00 / 26.33 |
| YOLO RTT mean/max | 219 / 301 ms | 249 / 335 ms |
| ncnn mean | 1621.539 ms | 1686.042 ms |
| 持续正常闭环 | 137 次 / 329.188 s | 尚无同规模统计 |

pcap 数量由抓包窗口长度决定，不作为性能比较。Zephyr 的即时 state MAE 为 132.33，
比其 manual 的 72.67 更差；该项不能包装成改善。

## 当前演示决策

RT-Thread 仍作为主演示，原因是已有 137 次持续闭环，现场证据更成熟。Zephyr 已经是
完整的第二 RTOS，不再是“只启动/只测周期”的占位方案。选择入口：

```sh
scripts/board/select-atk-task123-rtos.sh rtthread fp-rr
scripts/board/select-atk-task123-rtos.sh zephyr fp-rr
```

正式完整报告保存在：

```text
/home/huhu/atk-bringup/zephyr-task123-unified-20260824/UNIFIED_TASK123_REPORT.md
/home/huhu/atk-bringup/zephyr-task123-unified-20260824/RTTHREAD_VS_ZEPHYR.md
```

当前 Zephyr 是 AxVisor Guest，不是 RK3588 原生 Zephyr BSP。300×2 是快速 A/B，
不替代计划中的 `18,000×4` 夜间实验。

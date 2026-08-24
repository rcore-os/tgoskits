# OpenRace 2026 三任务最终评分映射

官网评分标准：<https://opencamp.cn/qcl/camp/OpenRace2026/stage/1>

本表区分“当前代码/证据技术完成度”和“当前提交包可直接交付度”。评分是内部
估计，不是官方最终分数。2026-08-24 的 RK3588 证据已经补齐 Task 1
RR/FP-RR、Task 2 双 Guest 协议与故障恢复、Task 3 真实 Guest 内 ncnn/YOLO
闭环以及 RT-Thread/Zephyr 双 RTOS 对比。Task 1 仍因“无 RK3588 原生
RTOS BSP”和“未改官方 `dev` 不具备可比调度器”保守计 `26–27/30`。

## 一、任务关系

```text
Task 2：StarryOS/RTOS UDP/IP + T2N1 可靠协议
              ↓
Task 3：StarryOS Guest 内 ncnn/YOLO → T2N1 CONTROL → RTOS 控制 → STATUS
              ↓
Task 1：在完整系统上改造 AxVisor 调度、IRQ、定时器和关键锁路径
```

当前集成工作树以官方 `dev` 为骨架，融合 Task 3 运行时和 Task 1 实时改造。
三项在官网评分中独立计分，不能重复计算同一证据。

## 二、主评分（100 分）

| 评分项 | 满分 | 当前保守分 | 收口目标 | 关键依据 |
|---|---:|---:|---:|---|
| 任务一：实时 RTOS 化 | 30 | 26–27 | 27–28 | `results/task1/task1-final-closure-20260820/`、`two-gap-closure-20260820.md` |
| 任务二：客户机间网络通信 | 25 | 25 | 25 | `docs/design/task2-dual-guest-network-final.md`、七类场景 pcap、RT-Thread/Zephyr 物理板证据 |
| 任务三：AI 控制闭环 | 25 | 25 | 25 | `docs/design/task3-ai-design.md`、`results/atk-dlrk3588-task123-integrated-ab-20260824/` |
| 工程完整性与文档 | 15 | 13–14 | 14–15 | 统一构建/验证入口、SHA256、pcap、串口日志和评分映射已齐；待完成最终 merge commit 与演示视频 |
| 系统创新与扩展性 | 5 | 4 | 4–5 | FP-RR、内部 L2 switch、可插拔协议/模型 |
| **主评分合计** | **100** | **93–95** | **95–97** | Task 1 按 26–27 计；不含加分项 |

### Task 1 细分

| 子项 | 分值 | 当前判断 |
|---|---:|---|
| 目标与关键路径分析 | 4 | 4：调度、WFI、timer、IRQ、锁、vCPU wake 均有路径分析 |
| AxVisor 关键机制实质改造 | 8 | 7（8 为乐观）：FP-RR、有界服务、per-CPU timer wheel、有界 vIRQ、IRQ-tail 顺序修复；任意 IRQ 抢占和物理板闭环仍有限制 |
| 多核 Linux 配置 | 4 | 4：2-vCPU、pCPU 绑定、内存、设备、IRQ 和启动参数齐全 |
| 改造前后与最坏情况数据 | 5 | 4：RK3588 上 RT-Thread RR→FP-RR 的 P99 下降 90.70%，Zephyr 快速臂下降 92.10%；仍不声称硬 WCET |
| idle/stress 对比 | 4 | 4：Linux idle/stress 各 20,000 样本，600 秒 soak 600,000 样本，并与 RTOS 同核竞争 |
| 原生 RTOS 基线 | 5 | 4：原生 Zephyr QEMU 可复现；RK3588 官方源树无可用裸机 BSP，该对照明确标为 BLOCKED |

### 两个评审缺口的当前状态

| 缺口 | 最近提交 | 当前结论 | 仍保留的限制 |
|---|---|---|---|
| 共核竞争与实验收尾 race | 当前集成实现 | **已解决**：Linux 使用 hold/release，runner 显式选择 console；RK3588 实验保留 20,000/600,000 样本与并行 RTOS 探针 | 实测不是硬 WCET 证明；未改官方 `dev` 仍无等价可比调度器 |
| IRQ 返回尾部抢占与 GIC completion 顺序 | 当前集成实现 | **已解决主要根因，采用有界实现**：completion/EOI 先于 guard 释放；固定优先级仅对严格更高优先级唤醒请求尾部抢占；RR/FIFO 保持原行为 | 不是“每个 IRQ 都抢占”；不声称所有架构、IRQ 源和物理板的硬实时界限 |

### Task 2 细分

| 子项 | 分值 | 当前判断 |
|---|---:|---|
| 双向 IP 网络链路 | 4 | 4：VirtIO-net、AxVisor switch、双向 UDP/IPv4 |
| 应用层协议 | 5 | 5：T2N1 版本、类型、长度、序号、ACK、错误码、CRC32 |
| 控制/状态/错误消息 | 5 | 5：CONTROL/STATUS/ERROR 及 session-mismatch 互操作语义均有回归 |
| 可靠性/超时/重传/恢复 | 4 | 4：ACK drop、重复、乱序、Safe、恢复均有证据 |
| 自动化测试数据 | 4 | 4：协议 21/21、Starry endpoint 11/11、Python 34/34，七类历史 pcap 场景 PASS，两种 RTOS 实际编译通过 |
| 隔离与访问控制 | 3 | 3：stage-2、DMA carveout、IRQ route、MAC/IP/session/CRC 检查 |

### Task 3 细分

当前五项均有实现和物理板结果：StarryOS Guest 内真实 ncnn/YOLO
推理、T2N1 输出、RTOS 可观察控制、同 request ID 状态回传、同侧 RTT
与误差边界、manual/YOLO 固定输入对比。三张有效图片完成 3/3
`CONTROL/STATUS`，两张拒绝图片完成 2/2 安全拒绝，保守计 `25/25`。

## 三、官网 10 分加分项

官网的 10 分不是 10 个各 1 分的小项，而是 3 类：

| 加分项 | 分值 | 当前状态 |
|---|---:|---|
| StarryOS 替代 Linux 完成相关任务 | 4 | 4：StarryOS Guest 完成 Task 2 网络与 Task 3 Guest 内 ncnn/YOLO 闭环 |
| StarryOS syscall 完善并合入 `dev` | 最多 4 | 0：当前没有该类合入 |
| 多种 RTOS 或多种开发板对比 | 2 | 2：同一 RK3588 上 RT-Thread 与 Zephyr 4.4.2 均完成 Task 1–3 |
| **加分合计** | **10** | **6** |

理论总上限是 `100 + 10 = 110`。按当前保守主分和已有 6 分加分，
内部估计为 `99–101/110`。StarryOS 和第二 RTOS 都有可观察闭环，
不是单纯启动证据；最终是否得分仍由官方评审判定。

## 四、当前必须收口的事项

1. 完成当前官方 `dev` 骨架上的 merge commit，并保存最终提交 ID。
2. 在不重建大型缓存、不干扰并行板卡工作的前提下，优先补一次最终
   commit 的系统闭环；若环境不允许，必须明确区分“当前代码验证”与
   “已归档物理板证据”。
3. 刷新最终 evidence manifest、复现命令和 SHA256 索引。
4. 录制官网要求的约 5 分钟演示视频。

StarryOS/STERRORS 与第二 RTOS/板卡的当前审计和外部阻塞见
`results/bonus-path-audit-20260821.md`；没有对应的可观察闭环就不计入加分。
当前不再扩大 Task 1 调度机制范围，也不再增加 fixture/replay
作为 Task 3 主路。优先顺序为：官方基线 merge 收口 → 最终回归与证据索引
→ 演示视频。

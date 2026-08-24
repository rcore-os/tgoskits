# OpenRace 2026 三任务最终评分映射

官网评分标准：<https://opencamp.cn/qcl/camp/OpenRace2026/stage/1>

本表区分“当前代码/证据技术完成度”和“当前提交包可直接交付度”。评分是内部
估计，不是官方最终分数。Task1 的两个评审缺口已经完成限定范围内的代码修复和
QEMU 闭环验证，但官方 `dev` 可比运行和物理板证据仍未补齐，因此提交时采用
`26–27/30` 的保守区间，`28/30` 仅作为乐观上限。

## 一、任务关系

```text
Task 2：Linux/RTOS UDP/IP + T2N1 可靠协议
              ↓
Task 3：Linux 推理 → T2N1 CONTROL → RTOS 控制 → STATUS
              ↓
Task 1：在完整系统上改造 AxVisor 调度、IRQ、定时器和关键锁路径
```

当前 `openrace/task1-rt-partition` 继承 Task 3 运行时，并叠加 Task 1 实时
改造。三项在官网评分中独立计分，不能重复计算同一证据。

## 二、主评分（100 分）

| 评分项 | 满分 | 当前保守分 | 收口目标 | 关键依据 |
|---|---:|---:|---:|---|
| 任务一：实时 RTOS 化 | 30 | 26–27 | 27–28 | `results/task1/task1-final-closure-20260820/`、`two-gap-closure-20260820.md` |
| 任务二：客户机间网络通信 | 25 | 24 | 25 | `book/design/task2-dual-guest-network-final.md`、Task 3 双侧 pcap |
| 任务三：AI 控制闭环 | 25 | 25 | 25 | `book/design/task3-ai-design.md`、`results/task3/` |
| 工程完整性与文档 | 15 | 11–12 | 14–15 | 已有统一 evidence manifest、演示 runbook、Task2 canonical 文档和当前 HEAD 故障证据；仍缺无冲突 PR/远程重放审计 |
| 系统创新与扩展性 | 5 | 4 | 4–5 | FP-RR、内部 L2 switch、可插拔协议/模型 |
| **主评分合计** | **100** | **90–92** | **96–98** | Task1 按 26–27 计；工程项因统一索引和可定位证据上调；不含加分项 |

### Task 1 细分

| 子项 | 分值 | 当前判断 |
|---|---:|---|
| 目标与关键路径分析 | 4 | 4：调度、WFI、timer、IRQ、锁、vCPU wake 均有路径分析 |
| AxVisor 关键机制实质改造 | 8 | 7（8 为乐观）：FP-RR、有界服务、per-CPU timer wheel、有界 vIRQ、IRQ-tail 顺序修复；任意 IRQ 抢占和物理板闭环仍有限制 |
| 多核 Linux 配置 | 4 | 4：2-vCPU、pCPU 绑定、内存、设备、IRQ 和启动参数齐全 |
| 改造前后与最坏情况数据 | 5 | 4：Zephyr 和 Linux max 明显改善，但 Linux P99 有 trade-off，官方 dev 运行未形成可比正式数据 |
| idle/stress 对比 | 4 | 4：四场景长稳矩阵和 Linux/Zephyr 同核竞争 |
| 原生 RTOS 基线 | 5 | 4：原生 Zephyr QEMU 可复现，但仅一种 RTOS、无物理板 |

### 两个评审缺口的当前状态

| 缺口 | 最近提交 | 当前结论 | 仍保留的限制 |
|---|---|---|---|
| 共核竞争与实验收尾 race | `78cb05c49 fix(rt-tests): hold Linux guest through result collection` | **已解决（QEMU 测试协议范围内）**：Linux 使用 hold/release，runner 显式选择 console，marker 顺序和 QMP 正常退出均有回归验证 | QEMU TCG 尾延迟仍有波动；没有物理板 worst-case；官方 `dev` literal runtime 仍不可比 |
| IRQ 返回尾部抢占与 GIC completion 顺序 | `6ac70bcab fix(rt): make IRQ-tail preemption priority-aware` | **已解决主要根因，采用有界实现**：completion/EOI 先于 guard 释放；固定优先级下仅严格更高优先级唤醒触发尾部抢占；RR/FIFO 保持原行为 | 不是“每个 IRQ 都抢占”；当前证据覆盖 AArch64 AxVM/QEMU，未证明所有架构、所有 IRQ 源和物理板硬实时界限 |

### Task 2 细分

| 子项 | 分值 | 当前判断 |
|---|---:|---|
| 双向 IP 网络链路 | 4 | 4：VirtIO-net、AxVisor switch、双向 UDP/IPv4 |
| 应用层协议 | 5 | 5：T2N1 版本、类型、长度、序号、ACK、错误码、CRC32 |
| 控制/状态/错误消息 | 5 | 5：CONTROL/STATUS/ERROR 及 session-mismatch 互操作语义均有回归 |
| 可靠性/超时/重传/恢复 | 4 | 4：ACK drop、重复、乱序、Safe、恢复均有证据 |
| 自动化测试数据 | 4 | 4：协议/controller 回归已接入默认 CI gate；完整双 Guest QEMU 仍由显式脚本和证据 job 运行 |
| 隔离与访问控制 | 3 | 3：stage-2、DMA carveout、IRQ route、MAC/IP/session/CRC 检查 |

### Task 3 细分

当前五项均有实现和结果：Linux 神经网络推理、T2N1 输出、RTOS 可观察控制、
状态回传闭环、同侧 RTT 和误差说明、AI/固定参数至少两项指标对比，保守计
`25/25`。

## 三、官网 10 分加分项

官网的 10 分不是 10 个各 1 分的小项，而是 3 类：

| 加分项 | 分值 | 当前状态 |
|---|---:|---|
| StarryOS 替代 Linux 完成相关任务 | 4 | 0：当前正式闭环使用 Linux |
| StarryOS syscall 完善并合入 `dev` | 最多 4 | 0：当前没有该类合入 |
| 多种 RTOS 或多种开发板对比 | 2 | 0：目前只有 Zephyr/QEMU 正式基线 |
| **加分合计** | **10** | **0** |

理论总上限是 `100 + 10 = 110`。增加第二个 RTOS 只争取最后一行的 2 分；
StarryOS 必须用它完成可观察的任务闭环，单纯启动不能拿满 4 分。

## 四、当前必须收口的事项

1. 将 Task2 canonical 设计/运行摘要和本评分表纳入最终分支。
2. 更新 Task1 旧设计与失败复核文档的“历史记录”标识。
3. 在当前 HEAD 上复跑一次 Task2 网络 + Task3 AI 闭环，避免只引用旧提交结果。
4. 统一构建、启动、验证命令和 SHA256 证据。
5. 在独立 review 分支逐项解决 Task2/Task3 与 `dev` 的 PR 冲突，并创建
   Task1 PR；当前已完成只读冲突审计和隔离试合并，实际解决仍需逐项回归，
   未将未验证合并带入最终证据分支。
6. 录制官网要求的约 5 分钟演示视频。

StarryOS/STERRORS 与第二 RTOS/板卡的当前审计和外部阻塞见
`results/bonus-path-audit-20260821.md`；没有对应的可观察闭环就不计入加分。
当前仍不再扩大 Task1 调度机制范围。优先顺序为：远程重放/PR 收口 →
StarryOS/STERRORS 可观察闭环（若外部资产可用）→ 第二 RTOS/板卡；Task1
补充实验只在低成本、能直接补分时执行。

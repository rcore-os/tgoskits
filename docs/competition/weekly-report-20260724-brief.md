# 智能化工控虚拟化混合系统 — 项目周报（简洁版）

- **报告周期**：2026-07-18 ~ 2026-07-24
- **工作分支**：`feat/rt-axvisor-partition-virtio-net`
- **赛题基线**：`competition/requirement.md`

---

## 一、本周进展

### 任务一：实时性改造
- 完成 Zephyr / Linux 改造前基线：空载稳定，压力下 worst-case 最高恶化 **89×**（Zephyr）、Linux cyclictest avg 恶化 **23.5×**。
- 实现 `dedicated_cpus` 专核隔离：非 RT VM 的 vCPU 被禁止调度到 RT 专核上。
- 启用抢占式 RR 调度器（`ax-std/sched-rr`），替代默认 FIFO 合作式调度。
- 多核 Linux 客户机可稳定运行，配置/构建/测试文档已更新。

### 任务二：客户机间通信
- 完成模拟 **virtio-net** 设备后端 + AxVisor 内部 **L2 软件交换机**。
- 打通 TX/RX virtqueue 数据通路；解决 RX 中断通过 physical-SPI pend 送达 guest。
- 两个 Linux 客户机实现 **双向 ICMP ping 互通**（VM1→VM2 0% 丢包，RTT 1–12 ms）。
- 实现应用层协议 `ivcproto`（UDP，16 B 头，支持 CONTROL/STATUS/ERROR/DATA/ACK）。
- 可靠性机制：ACK + 超时重传 + 去重/乱序容忍 + checksum；回环验证 40 条消息 0 丢失。

### 任务三：AI 控制联动
- 尚未启动，计划任务二稳定后展开。

---

## 二、关键问题

| 问题 | 状态 | 说明 |
|---|---|---|
| 2-VM 同时引导不稳定 | 根因已定位 | 合作式调度 + passthrough timer 导致 guest 态 vCPU 不可抢占；次级 pCPU bring-up 存在时序竞争 |
| Zephyr 网络镜像 | 待办 | 本机无 Zephyr SDK，需构建带 virtio-net + lwIP 的镜像 |

---

## 三、下周计划

1. **攻关 2-VM 稳定同时引导**（目标：单次成功率 ≥80%）。
2. **构建 Zephyr 网络镜像**，替换 ivcproto 一端进行跨 OS 验证。
3. **完善自动化测试**：成功率、延迟、吞吐、异常恢复指标采集。
4. **任务三预研**：选定 AI 框架与轻量模型。

---

## 四、交付物

- `docs/realtime/preemptive-scheduling.md`
- `docs/realtime/M1-zephyr-baseline.md`
- `docs/ivc/M5-network-design.md`
- `docs/ivc/ivcproto/README.md` 及源码
- 本周报：`docs/competition/weekly-report-20260724-brief.md`

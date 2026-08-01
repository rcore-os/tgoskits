# 智能化工控虚拟化混合系统部署及联动实现 — 项目周报

> 报告周期：2026-07-18 ~ 2026-07-24（截至 baseline dev 分支当前进度）  
> 工作分支：`feat/rt-axvisor-partition-virtio-net`（基于 `upstream/dev`）  
> 赛题来源：`competition/requirement.md`

---

## 1. 本周总体进展

本周围绕赛题三大任务继续推进，重点完成了任务一的调度实时化改造（partition + 抢占式 RR）、任务二的模拟 virtio-net + 软件 L2 交换机数据通路，以及跨客户机应用层协议与可靠性机制的实现。当前两个真实 Linux 客户机已通过自研虚拟网卡实现**双向 ICMP ping 互通**，应用层协议在单机和真实网络栈上均完成验证；2-VM 同时引导的稳定性瓶颈已定位到 AxVisor 并行 bring-up / 合作式调度层面，正作为后续首要攻关点。

| 任务 | 目标 | 当前状态 | 关键交付物 |
|---|---|---|---|
| 任务一：实时性改造与验证 | 优化 AxVisor 调度/抢占/亲和性，启动多核 Linux，建立 RT 基线 | **调度改造已落地，基线已采集，多核 Linux 可稳跑** | `docs/realtime/preemptive-scheduling.md`、`docs/realtime/M1-zephyr-baseline.md` |
| 任务二：客户机间通信 | Linux/Starry ↔ RTOS 间建立 IP 链路 + 应用层协议 | **数据通路全打通，双向 ping 成功，协议实现并验证** | `docs/ivc/M5-network-design.md`、`docs/ivc/ivcproto/` |
| 任务三：AI 模型与控制联动 | Linux 侧 AI 推理 → 网络协议 → RTOS 控制动作 | **尚未启动**，待任务二双客户机稳定后展开 | — |

---

## 2. 任务一：实时性改造与验证

### 2.1 已完成工作

1. **基线测量（M1）**
   - 在 QEMU aarch64/TCG 下采集了 Zephyr `latency_measure` 官方基准的多次启动数据（N=12），建立了空载 vs 宿主 CPU 压力对照。
   - 关键发现：空载下 context-switch 类延迟稳定在 600–1100 ns、jitter <130 ns；**压力下 worst-case max 最高膨胀约 89×**（`semaphore.take.immediate` 138 ns → 12.3 µs；`heap.malloc` 3.4 µs → 125 µs）。
   - 补充了多核 Linux 客户机的周期任务抖动基线：自写静态 musl `cyclictest` 探针（1 ms 周期，2 vCPU，各 10 000 次），空载 max 2.8 ms，压力（32 燃烧器）下 max 25.4 ms、avg 恶化 23.5×。
   - 文档与数据：`docs/realtime/M1-zephyr-baseline.md`、`tmp/m1/data/`。

2. **partition 调度改造（M2 路径 A）**
   - 在 VM 配置中新增 `[base] dedicated_cpus = true`：标记该 VM 的 pCPU 为独占，其他非 dedicated VM 的 vCPU task 在创建时会被排除出这些 pCPU。
   - 覆盖了固定 vCPU 与未固定 vCPU 两种情况，防止普通任务漂移到 RT 专核。
   - 改动范围：`virtualization/axvmconfig`、`virtualization/axvm`、`os/axvisor`，共 6 个文件、+93/-4 行。
   - 验证：编译、clippy、fmt 全绿；单 VM 多核 Linux cyclictest 正常启动运行，行为无回归。

3. **抢占式 RR 调度器启用**
   - AxVisor 默认使用 ArceOS 的 FIFO 合作式调度（`task_tick` 返回 false，vCPU 不被抢占）。
   - 通过启用 `ax-std/sched-rr` feature，使 AxVisor 运行在**基于时间片（MAX_TIME_SLICE = 5）的抢占式 RR 调度器**上，为 RT 行为奠定软件基础。
   - 提供专用构建配置：`docs/realtime/axvisor-qemu-aarch64-preempt-rr.toml`。
   - 已在该配置下验证 Linux 客户机可正常引导到用户态、eth0 可用、应用服务可运行。

### 2.2 待继续工作

- 在真实板卡（OrangePi-5-Plus / RDK S100P）上采集改造前后的 RT 定量数据；QEMU/TCG 仅能做相对对比，不能作为 RT 保证依据。
- 补充 hypervisor 侧打点：trap 入口 → vCPU resume、IRQ 注入延迟。
- 长稳测试（≥数小时）与 stress-ng/hackbench/fio 等价压力场景下的 worst-case 延迟对比。
- Zephyr 侧带网络栈的 cyclictest 式周期任务 app（需 Zephyr SDK）。

---

## 3. 任务二：客户机间通信

### 3.1 已完成工作

1. **模拟 virtio-net 设备模型 + 软件 L2 交换机**
   - 在 `virtualization/axdevice` 中新增 `VirtioNet` 后端，实现 virtio-mmio v2 寄存器状态机、split virtqueue 解析、TX/RX 数据通路。
   - 解决 MMIO 陷入问题：从 passthrough 映射中 carve-out emu 设备页，使 guest 访问正确陷入到 AxVisor 模拟设备。
   - 实现 AxVisor 内部软件 L2 交换机：按目的 MAC 转发/广播以太帧，从一侧 VM 的 TX virtqueue 投递到另一侧 VM 的 RX virtqueue。
   - 解决 RX 中断送达：在 `interrupt_mode="passthrough"` 下，emu 设备改为在物理 GICD 上 pend 对应 SPI，使 guest 通过物理 CPU 接口收到 RX 中断；guest `/proc/interrupts` 中 INTID 56 计数随 TX 活动递增，验证通过。
   - 支持 per-VM kernel cmdline 覆盖、per-VM `excluded_devices` 盘槽隔离，使两个 Linux guest 可同时从各自独立磁盘引导。

2. **双向互通验证**
   - 两个真实 Linux 客户机（VM1: 10.0.0.1，VM2: 10.0.0.2）通过模拟 virtio-net + 软件交换机实现**双向 ICMP ping 成功**：VM1 → VM2 10 发 10 收 0% 丢包，VM2 → VM1 10 发 9–10 收，RTT 1–12 ms。
   - 关键突破：physical-SPI pend 中断送达、per-VM disk 隔离、per-VM cmdline、gppt 解决双 guest 抢物理 GIC 导致的中断丢失。
   - 详细记录：`docs/ivc/M5-network-design.md`。

3. **应用层协议与可靠性机制（ivcproto）**
   - 在 `docs/ivc/ivcproto/` 实现了一套基于 **UDP/IP** 的应用层协议，含 16 B 头部：magic/version/msg_type/seq/timestamp_ms/payload_len/checksum。
   - 消息类型覆盖任务要求的 **控制指令（CONTROL）、状态回传（STATUS）、错误通知（ERROR）**，以及 DATA/ACK。
   - 可靠性机制：ACK + 400 ms 超时 + 最多 6 次重传、按 seq 去重/容乱序、checksum 完整性校验。
   - 提供 `ivcproto server` / `ivcproto client` 两端程序，以及 `guest-init.sh`、`build-rootfs.sh` 自动化脚本；server 支持 `lossy=K` 模式主动丢 ACK，用于压力测重传/去重路径。
   - 回环验证：`sent=40 acked=40 lost=0 retransmits=8`、`unique=40 dups=8 corrupt=0 acks_dropped=8`，自洽；真实网络栈上 server 已进入监听状态。

### 3.2 当前阻塞与风险

- **2-VM 同时引导仍不稳定**：虽然网络数据通路本身已 100% 工作（一旦双 guest 都起来，ping 0% 丢包），但多次运行中能同时 boot 到 init 的概率较低（约 3/80）。
- **根因已定位**：
  1. 合作式 FIFO 调度器下，同 pCPU 的 vCPU 无法被抢占，噪声 VM 会饿死 RT VM；
  2. passthrough timer 模式下，正在 guest 态执行的 vCPU 拥有物理定时器，宿主 timer tick 不触发，RR 调度器也无法抢占 guest 态 vCPU；
  3. 分到不同 pCPU 时，次级 pCPU 的 vCPU bring-up 存在时序竞争。
- **结论**：稳定 2-VM 同时引导需要修改 AxVisor 并行 VM / 次级 pCPU vCPU bring-up 路径，或实现 guest-timer 模拟以支持 vCPU 抢占，属于架构级改动。

### 3.3 待继续工作

- 攻关 2-VM 稳定同时引导（优先路径：guest-timer 模拟 / 改进 bring-up 时序）。
- 将 ivcproto 一端替换为 Zephyr（需安装 Zephyr SDK，构建带 virtio-net + lwIP/BSD socket 的镜像）。
- 自动化可靠性/性能测试：请求成功率、应用层错误、超时、异常恢复、请求-响应延迟、有效应用吞吐。
- 完善网络拓扑文档：MAC/IP、路由、端口、NAT/桥接、访问控制策略。

---

## 4. 任务三：AI 模型与控制联动

- **尚未启动**。
- 计划：待任务二两个客户机（Linux/Starry + Zephyr）稳定互通后，在 Linux/Starry 侧部署轻量神经网络推理（TFLite / ONNX Runtime / ncnn 之一），通过 ivcproto 将模型输出发送给 Zephyr；Zephyr 根据 AI 输出调整 PID/控制策略，驱动虚拟外设或数值仿真被控对象，并将状态回传，形成闭环。
- 端到端延迟拟采用同侧往返 RTT 测量，避免跨客户机时钟同步问题；演示场景以固定参数手动控制为基线，对比 AI 参与后的响应延迟、控制误差等指标。

---

## 5. 工程规范执行

- 所有构建/运行/测试均通过 `cargo xtask` 完成。
- 代码修改后均执行了对应 crate 的 `cargo xtask clippy` 与 `cargo fmt`。
- 未使用 `#[allow]` 掩盖 clippy 警告；已修复根因。
- 新增文档均按仓库风格以 Markdown 维护，关键结论均附复现命令与日志路径。

---

## 6. 下周计划

| 优先级 | 任务 | 目标/验收标准 |
|---|---|---|
| P0 | 稳定 2-VM 同时引导 | 实现可重复的双客户机同时 boot 到 init，单次运行成功率 ≥80%，从而拿到 ivcproto 端到端交换日志 |
| P1 | Zephyr 网络镜像 | 安装 Zephyr SDK，构建带 virtio-net + lwIP/BSD socket 的 `qemu_cortex_a53` 镜像，替换 ivcproto 一端 |
| P2 | 自动化测试脚本 | 完成请求-响应成功率、延迟、吞吐、异常恢复等指标的自动化采集 |
| P3 | 任务三预研 | 选定 AI 框架与轻量模型，准备 Linux/Starry 侧推理 demo 与控制仿真场景 |

---

## 7. 参考资料

- 赛题要求：`competition/requirement.md`
- 总体规划：`docs/rt-axvisor-plan.md`
- 实时性改造与基线：`docs/realtime/preemptive-scheduling.md`、`docs/realtime/M1-zephyr-baseline.md`
- 网络底座与互通：`docs/ivc/M5-network-design.md`
- 应用层协议与测试：`docs/ivc/ivcproto/README.md`、`docs/ivc/ivcproto/src/main.rs`

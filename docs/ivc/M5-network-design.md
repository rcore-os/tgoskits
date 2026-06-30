# 任务二 · M5 客户机间网络底座设计与可行性（QEMU aarch64）

> 任务二（客户机间 IP 通信）的设计/可行性里程碑（M5）。基于对 `axdevice` 设备模拟框架的实地勘察，给出 virtio-net 设备模型 + 软件 L2 交换机的实现路径、三大硬前置、风险与分阶段计划。
> 日期：2026-06-30。状态：设计完成，实现未开始。

## 1. 目标回顾

在 Linux 客户机 ↔ Zephyr 客户机之间建立**基于 IP 协议栈的双向网络链路**（虚拟网卡/桥接/用户态网络/实际网口）。共享内存、HyperCall、裸 MMIO 门铃**不得作主数据通道**。推荐方案：**模拟 virtio-net + 软件 L2 交换机**。

## 2. 框架勘察结论（已核实）

### 2.1 设备模拟框架（`virtualization/axdevice*`）
- **设备接口**：`axdevice_base::BaseDeviceOps<R>`（`BaseMmioDeviceOps = BaseDeviceOps<GuestPhysAddrRange>`）：
  - `emu_type() -> EmuDeviceType`
  - `address_range() -> R`
  - `handle_read(addr, width) -> AxResult<usize>`
  - `handle_write(addr, width, val) -> AxResult`
- **工厂/注册**：`DeviceFactory::build(config, ctx) -> AxResult<DeviceBundle>`；`DeviceFactoryRegistry`；`register_builtin_factories()`（`axdevice/src/factory.rs`）**当前只注册了 `MetaDeviceFactory`**。
- **MetaDeviceFactory 已实现**：InterruptController / Console / **IVCChannel** / GPPT{Distributor,Redistributor,ITS} / FwCfg / LoongArchPchPic / X86IoApic / X86Pit / PPPTGlobal。
- **枚举槽已就绪**：`EmulatedDeviceType::VirtioNet = 0xE2`、`VirtioBlk = 0xE1`、`VirtioConsole = 0xE3` 均已定义，但**无后端实现**。
- **emu_devices 配置格式**（toml `[devices].emu_devices`）：`[name, base_ipa, ipa_len, alloc_irq, emu_type, emu_config]`。

### 2.2 关键缺口（R1，需从零实现）
1. **无 virtio-net 设备后端**：只有枚举值，无 `BaseMmioDeviceOps` 实现。
2. **无设备侧 virtqueue 脚手架**：仓库的 `virtio_drivers` 是**驱动侧**（guest 消费 virtio）；**设备侧**（后端：解析 descriptor/avail/used ring、DMA 访问 guest 内存）需自写。
3. **无软件交换机**：`vmm/ivc.rs` 近乎空壳；`IVCChannel` 是共享内存/门铃类（**不满足 IP 要求**，只能作辅助）。

## 3. 目标架构

```
 Linux guest                              Zephyr guest
 ┌──────────────┐                         ┌──────────────┐
 │ virtio-net   │                         │ virtio-net   │
 │  driver(eth0)│                         │  driver      │
 └──────┬───────┘ MMIO trap               └──────┬───────┘
        │ (virtio-mmio regs + virtqueue)         │
 ┌──────▼─────────────── AxVisor ────────────────▼──────┐
 │ VirtioNetDevice(VM1)   ←—— L2 软交换机 ——→  VirtioNetDevice(VM2) │
 │  (BaseMmioDeviceOps)      MAC 学习/广播       (BaseMmioDeviceOps) │
 │                          可选 uplink → net/ax-net(smoltcp) 宿主栈 │
 └──────────────────────────────────────────────────────┘
```

- 每个 VM 配置一个 emu virtio-net 设备（virtio-mmio 传输，base_ipa/irq 由 emu_devices 指定）。
- AxVisor 内一个全局**软件 L2 交换机**：从一侧 virtio-net 的 TX virtqueue 取帧，按目的 MAC 转发到另一侧的 RX virtqueue（MAC 学习 + 广播/组播泛洪），并按需注入 RX 中断。
- 可选 uplink 到 `net/ax-net`（smoltcp 系，含 tcp/udp/router/dhcp），让客户机也能访问宿主/外网。

## 4. 实现路径（分阶段）

> **进度（2026-06-30）**：P1 设备后端骨架**已实现并编译/clippy/fmt 通过**——`virtualization/axdevice/src/virtio_net.rs`（`VirtioNet` 实现 `BaseMmioDeviceOps<GuestPhysAddrRange>`，virtio-mmio v2 寄存器状态机：magic/version/deviceid=1/vendor、特性协商 advertise `VIRTIO_NET_F_MAC`+`VIRTIO_F_VERSION_1`、QueueSel/Num/Ready/Desc/Driver/Device 地址、Status、InterruptStatus/ACK、net config MAC[6]）；在 `device.rs::init()` 的 `EmulatedDeviceType::VirtioNet` 臂注册（`MmioDeviceAdapter::from_arc`，MAC=52:54:00:00:00:NN，NN 取 cfg_list[0]）；`is_legacy_fallback` 与 `lib.rs mod virtio_net` 已加。**QueueNotify 暂只接受不处理 virtqueue（P2/P3 再做）。**
> **剩余 P1 阻塞（让 Linux 真正看到 eth0）**：AxVisor 的 guest FDT 生成**不发射 virtio-mmio 节点**（`os/axvisor/src/fdt/parser.rs` 仅做 emu_devices 地址重叠检查，无节点创建）。需在 FDT 生成处加 `virtio_mmio@<base> { compatible="virtio,mmio"; reg=<base size>; interrupts=<...>; }`，guest Linux 才能探测。这是 P1 的下一步。

- **P1 设备侧 virtqueue + virtio-mmio 寄存器状态机**：✅ 寄存器状态机已实现。待办：split virtqueue（desc/avail/used）解析 + 经 stage-2 访问 guest 物理内存收发缓冲（P2）；guest FDT virtio-mmio 节点发射（让 Linux 探测）。
- **P2 VirtioNetDevice + 工厂注册**：`impl BaseMmioDeviceOps`；`VirtioNetFactory: DeviceFactory { device_type()=VirtioNet }`；在 `register_builtin_factories` 注册；TX 路径把帧交给交换机，RX 路径把帧入队 + 注入 IRQ。
- **P3 软件 L2 交换机**：全局表（VM/端口 ↔ MAC），转发逻辑，线程/poll 驱动。先做两端点直连，再加 MAC 学习。
- **P4 客户机驱动接通**：Linux 侧 virtio-net 驱动自动识别（已内建），配 IP；Zephyr 侧需带 virtio-net + lwIP/BSD socket 的镜像（见前置 C）。
- **P5 应用层协议 + 可靠性 + 自动化测试**（M7/M8）：见任务二后续。

## 5. 三大硬前置（都需可观搭建，是 Task 2 的真实门槛）

| # | 前置 | 现状 | 影响 |
|---|---|---|---|
| **A. virtio-net 后端 + 软交换机** | 从零（本设计 P1–P3） | 最大工程，数千行级 | 阻塞全部 |
| **B. 2-VM 在 QEMU 共存** | ✅ **已验证可行（2026-06-30）**：Linux(id=1,pCPU0,passthrough) + Zephyr(id=2,pCPU1,passthrough) 同时 `--vmconfigs` 启动，**两者均 boot success 并各自跑完负载**（Zephyr 线程基准 + Linux cyclictest avg 121.9µs）。`--vmconfigs A --vmconfigs B`，各 VM 钉不同 pCPU。证据 `tmp/m2/coexist.log` | **不再是阻塞** | 已解除 |
| **C. 带网络的 Zephyr 镜像** | 预编译 Zephyr 镜像**无网络栈**（ELF 无 virtio_net/eth 符号）；需 Zephyr SDK + west 从源码构建带 lwIP/virtio-net 的镜像，**本机无 SDK** | 中等，需装 SDK（数 GB）+ 配置 | 阻塞 Zephyr 端点 |

> 备选降级：若 Zephyr 镜像短期无法带网络，可先用**两个 Linux 客户机**（各带独立 rootfs/磁盘）验证 virtio-net + 软交换机 + 应用协议，再替换一端为 Zephyr。但两 Linux 仍需前置 B（2-VM 共存）。

## 6. 风险

- **R1（最高）**：virtio-net 后端 + 软交换机从零，virtqueue/DMA/中断注入正确性复杂。缓解：P1 先做最小可用（单队列、无 offload、轮询），用 Linux guest 自带驱动当对端验证。
- **R-B（已大幅降低）**：2-VM 共存已验证可行（passthrough 模式、各钉不同 pCPU 即可，无需模拟 GIC）。后续若两 Linux 端点需各自独立磁盘，注意 disk0 共享问题（需第二块 -drive + 设备映射）。
- **R-C**：Zephyr 构建链缺失。缓解：先装 Zephyr SDK + west 单独验证能 build 带 net 的 qemu_cortex_a53 镜像；或先用两 Linux 走通全链路。
- **TCG 不限制本任务**：任务二是纯软件功能（连通性/协议/吞吐），QEMU TCG 完全够用——与任务一的 RT 定量限制不同，这是优先做任务二的理由。

## 7. 下一步（实现起点）
1. ~~前置 B~~ ✅ 已验证 2-VM 共存可行（见上）。
2. **P1 起步（关键路径）**：在 `virtualization/axdevice/` 新建 `virtio_net.rs`，实现 virtio-mmio 寄存器状态机 + 最小 split virtqueue，注册 `VirtioNetFactory`，先让 Linux guest 的 virtio-net 驱动探测到设备（dmesg 看到 `virtio_net` / `eth0`）。
3. 接上软交换机（P3）→ 两 Linux guest（各独立磁盘）互 ping / 跑应用协议（M7/M8）。
4. 端点 Zephyr 化：装 Zephyr SDK 构建带 virtio-net + lwIP 的镜像（前置 C），替换一端。

> 实测增量：2-VM 共存无需模拟 GIC（passthrough + 各钉不同 pCPU 即可），前置 B 比预想简单。剩余关键路径 = P1–P3（virtio-net 后端 + 软交换机），前置 C（Zephyr 带网络镜像）可用"先两 Linux"绕过到最后再做。

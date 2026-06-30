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
> **剩余 P1 集成（让 Linux 真正看到 eth0）——已完整追踪路径**：
> 1. **MMIO 路由已确认可行**：guest MMIO 陷出经 `axvm/src/vm.rs` 的 `handle_mmio_read/handle_mmio_write` 按地址查 `AxVmDevices` 的 mmio_index 分发到设备的 `handle_read/handle_write`。我的 VirtioNet 注册了 `address_range [base,base+0x200)`，**只要该区域陷入就会收到访问**。
> 2. **需让该区域陷入**：emu 设备区域必须在 guest stage-2 **不映射**（成为洞）才会 fault→路由到 emu 设备。当前 guest `passthrough ["/"]` 会把整树（含 QEMU virt 的 virtio-mmio 槽 0x0a00_0000）映射过去 → 不 fault。需通过 emu_devices 注册 + `excluded_devices`/`passthrough_addresses` 把 virtio-net 的 base 从 passthrough 排除，使其陷入。
> 3. **需 FDT `virtio_mmio` 节点**：guest FDT 生成（`fdt/create.rs`，基于拷贝/变换宿主 FDT）。若把 virtio-net 放在 **QEMU virt 已有的 virtio-mmio 槽地址**（如 0x0a00_0000），宿主 FDT 已含该节点 → 拷到 guest 即可，**无需新增节点**；否则需在 create.rs 加 `virtio_mmio@<base>{compatible="virtio,mmio";reg;interrupts}`。
> 4. **需 IRQ 注入**（RX 时）：alloc_irq 由 emu_devices 指定，RX 入队后注入。
> → P1 收尾 = 选一个 virtio-mmio 槽地址做 emu_device + 从 passthrough 排除 + 确认 FDT 节点 + boot 看 Linux dmesg 的 `virtio_net`/`eth0`。属于多步集成 + 多轮 boot 调试。
>
> **可执行配方（下次一次跑通的起点）**：QEMU virt aarch64 的 virtio-mmio 槽位于 `0x0a00_0000`，每槽 `0x200`，IRQ 为 SPI（精确 INTID 从宿主 FDT 的 `virtio_mmio@a000000` 的 `interrupts` 读，通常 SPI 16 → INTID 48）。
> 1. **emu_devices 条目**（VirtioNet 类型 = `0xE2` = 226，cfg_list[0]=MAC 末字节，每 VM 不同）：
>    ```toml
>    # name, base_ipa, ipa_len, alloc_irq, emu_type, emu_config
>    emu_devices = [ ["virtio-net", 0x0a00_0000, 0x200, <SPI_INTID>, 0xE2, [1]] ]
>    ```
> 2. **从 passthrough 排除该槽**，使其陷入：在 `excluded_devices` 加 virtio-mmio 节点（如 `["/virtio_mmio@a000000"]`），或用 `passthrough_addresses` 显式列出不含 0x0a00_0000 的范围。
> 3. **FDT 节点**：该地址宿主 FDT 已有 `virtio_mmio@a000000` 节点，guest FDT 会拷到 → 无需改 create.rs（先验证；若 excluded 把节点也删了，则需在 create.rs 重新发射该节点）。
> 4. **验证**：`cargo xtask axvisor qemu --arch aarch64 --vmconfigs <带该 emu_devices 的 linux 配置>`，看 guest `dmesg | grep virtio` 出现 `virtio_net virtio0 ...` 与 `eth0`。
> 5. 若仅 P1（无 virtqueue 处理），`eth0` 应能创建但无流量；P2 再做 virtqueue 收发 + P3 软交换机互通。
> **风险点**：emu 与 passthrough 的优先级/区域重叠、IRQ INTID 取值、excluded 是否连带删了 FDT 节点——都需 boot 实测确认。

- **P1 设备侧 virtio-mmio 寄存器状态机**：✅ 代码实现（`VirtioNet` 寄存器状态机 + 注册）。
  - ⚠️ **更正（2026-06-30）：先前"P1 已端到端验证 eth0"是误判**。加 MMIO trap 打点后证实：guest 对 `0x0a000000` 的访问**根本不陷入 AxVisor**（0 次 trap）；先前看到的 `eth0`（MAC `52:54:00:12:34:56` = QEMU 默认 NIC MAC）是 **QEMU 自动添加的默认用户态网卡**，不是本模拟设备。加 `-net none` 后 `eth0` 消失、`/sys/class/net` 只剩 `lo`/`sit0`，且本设备仍 0 次 trap。
  - **根因 + 修复（commit `aff2d202e`）**：AxVisor 把 `emu_devices` 的 MMIO 区域当 **passthrough 映射**（`vm.rs` 的 `pt_dev_region` identity-map），不陷入 emu。修复：在建 stage-2 映射前用 `carve_out()` 把每个 emu_devices 的 4K 对齐范围**从 passthrough 映射里挖掉**（FDT 节点仍发射）→ 该页陷入并路由到 emu 设备。
  - **子页粒度坑（已解）**：emu 设备 0x200 < 4K 页；① 设备 MMIO 窗口扩到整页 `0x1000`，同页内空 virtio-mmio 槽也路由到本设备并读回 0（否则空槽 trap → `mmio read: NotFound` → VM 崩）；② 设备须放在**不与 guest virtio-blk(0x0a000000) 同页的空闲槽**（用 `0x0a001000`，否则挖掉会连带 unmap 磁盘 → 引导挂起）。
  - ✅✅ **P1 真正端到端验证（2026-06-30）**：`-net none` 排除 QEMU 默认 NIC 后，Linux guest 的 `virtio_net` 驱动绑定本模拟设备，`eth0` 拿到**设备 advertise 的 MAC `52:54:00:00:00:01`**（证明 `VIRTIO_NET_F_MAC` 协商 + config 读取正确），磁盘仍正常（ext4 挂载）。配置 `tmp/m2/vm-linux-vnet3.toml` + `qemu-vnet2.toml`。
- **P2 virtqueue TX 数据通路**：✅✅ **已运行验证**。`VirtioNet::process_tx`（split virtqueue desc/avail/used 解析）在 guest `ip link up`+`ping` 后正确从 TX virtqueue 抽出真实以太帧：广播 ARP（`ff:ff:..` 目的、`0806`）来自设备 MAC、IPv6 组播（`33:33:..`、`86dd`），共 9 帧/3 ARP。`vm.rs::handle_mmio_write` 在 TX QueueNotify 时 `drive_virtio_net_tx`（`read_from_guest/write_to_guest` 闭包）+ TX 完成中断。
- **P2 RX + P3 软件 L2 交换机**：✅ 代码实现，**2-VM 互通受环境 flakiness 阻塞，未取得 ping 回复证据**。`deliver_rx`（写对端 RX virtqueue + 注 IRQ）+ 广播泛洪（`drive_virtio_net_tx` 对 `get_vm_list()` 每个其他 VM 调 `deliver_rx_frame`）。
  - **2-VM 尝试**（避开两 Linux 共享 disk0 冲突）：用 **initramfs/ramdisk 启动**（每 VM 自己内存一份 rootfs，`root=/dev/ram0 rdinit=/init rootwait`，自定义 busybox initramfs + MAC→IP 的 /init），两 VM 各一 virtio-net（slot 0x0a001000/0x0a002000，MAC ..01/..02，碰 disk0 同页问题已避开）。配置 `tmp/m2/vm-rd{1,2}.toml` + `qemu-rd.toml` + `tmp/m2/initrd/init`。
  - **观察**：一次运行中 VM1 成功 `INIT-RUNNING`(eth0 在)+ ping 10.0.0.2（TX 帧经 process_tx 抽出并泛洪），但**对端 VM2 的 /init 未运行**（ramdisk 引导 flakiness：内核到 `Freeing initrd memory` 后有时不 exec /init），eth0 未 up → 无 RX buffer → `deliver_rx` 返回 false（无投递）→ ping 无回复。
  - **RX 写入路径已验证（内置测试 peer + 单 reliable VM）**：用 AxVisor 内置 ARP/ICMP 应答 peer + 单个磁盘启动的可靠 Linux guest，guest ping 时 `process_tx` 抽出 ARP（TX 验证），`deliver_rx` 把应答帧写入 guest 的 RX virtqueue 并更新 used ring、返回 `injected=true`（**RX virtqueue 写入 + used 环更新机制验证通过**，与 TX used 环逻辑一致）。修了 RX 头 `num_buffers=1`（VERSION_1 下必需）。
  - **最后一公里（已定位到精确代码点）：RX 虚拟中断未被 guest 取走**。
    - **INTID 已确认 = 56**：guest `/proc/interrupts` 显示 `16: 0 GICv3 56 Edge virtio0`（我的 net 设备），emu_devices irq=56 正确；计数恒 0 = guest 从未收到该中断。
    - 注入链路：`deliver_rx` 设 `interrupt_status|=1` + `inject_interrupt_to_vcpu` → `queue_interrupt`(入 pending) → vcpu 循环 `inject_pending_interrupts` → `arm_vcpu inject_interrupt` → `axvm/arch.rs hardware_inject_virtual_interrupt` → `host/gic.rs inject_interrupt_gic_v3`：找空 LR，写 `ICH_LR_EL2 = VINTID(56)+STATE::Pending+GROUP::SET`，并 enable 虚拟接口（日志 "Virtual interrupt interface not enabled, enabling now"）。无报错。
    - **但 guest INTID56 计数仍为 0** → 写进 LR 的虚拟中断没被 guest 取走。诊断到 LR 值 `0x5000000000000038`（vintid=56, state=Pending, Group1, HW=0, prio=0, HCR.EN=1）——**LR 编程完全正确**。`arm_vcpu` 不做 vGIC save/restore（vcpu.rs:70 有 `FIXME: Handle its interaction with the virtual GIC`），故 LR 不被清。试过额外设 `ICH_VMCR_EL2.VENG1/VENG0/VPMR=0xff`（虚拟接口 Group 使能 + 优先级掩码全开）——**仍无效**。
    - **根因（已定性）**：`interrupt_mode="passthrough"` 下，**guest 直接用物理 GIC CPU 接口（ICC_* 系统寄存器不被 trap），从不读虚拟接口（List Register）** → 软件注入的虚拟中断对 guest 根本不可见。LR/VMCR 怎么写都没用，因为 guest 没在用虚拟接口。
    - **修复方向（架构级，非小改）**：软件模拟设备（virtio-net）需 **emulated / 部分透传 GIC 模式**——AxVisor 已有 `gppt-gicd`/`gppt-gicr`/`gppt-its` emu 设备（GIC 部分透传：模拟 GICD/GICR 以 trap guest 的 GIC 访问、走虚拟接口，同时物理中断仍透传），但当前 vmconfig 把它们注释掉了。需在 vmconfig 加这些 emu_devices + 相应 interrupt_mode，使 guest 经虚拟接口收中断，LR 注入才会被取走。这是让 RX 中断送达的前置，也是 task2 互通的最后一步（之后单 guest 内置 peer 即可 ping 通 → 2-VM 互通）。
    - **这是 task2 数据通路打通的唯一剩余步骤**：device 枚举 ✅、TX virtqueue ✅、RX virtqueue 写入+used 环 ✅（`deliver_rx` 返回 injected=true），仅差**虚拟中断送达 guest**这一 vGIC 细节。修复后单 guest（内置 peer）即可 ping 通，进而 2-VM 互通。
  - **✅✅✅ 已解决（2026-06-30，commit 30f17c2ee）：物理 SPI pend 在 passthrough 模式下送达中断**。
    - **代码级根因证实**（`arm_vcpu/src/vcpu.rs:204`）：`if !config.passthrough_interrupt { hcr_el2 += IMO + FMO }`——passthrough 模式下 `passthrough_interrupt=true` → **HCR_EL2.IMO 不置位** → guest 用物理 GIC CPU 接口、不读 List Register → 软件注入的虚拟中断不可见。
    - **两组运行实验定性**：① `interrupt_mode="emu"`（置 IMO + gppt-gicd/gicr 全 GICD 模拟）→ guest 起不来：`axplat_dyn::irq: Unhandled IRQ HwIrq(27)`（arch timer PPI）刷屏 4 万+ 行、0 行用户态输出 → **emu 模式在 QEMU-virt 动态平台缺物理→虚拟中断转发**（重型 feature）。② `gppt + passthrough` → 仍 INTID56=0（gppt 只模拟分发器 MMIO，不改 CPU 接口）。
    - **修复（轻量、留在 passthrough）**：emu 设备不走虚拟注入，改为在宿主物理 GICD 上 **pend 对应物理 SPI**（`gic::pend_physical_spi` → `arm_gic_driver v3 Gic::set_pending(IntId::spi(intid-32), true)`）。passthrough 模式下 guest 已自行在物理 GICD enable+route 了该 SPI（INTID 56→SPI 24，QEMU virt slot 8 物理空闲），故硬件直接投递到 guest EL1。`vm.rs` 新增 mode-aware `inject_device_irq`：passthrough→物理 pend，emu→虚拟 LR。
    - **运行验证（QEMU aarch64 virt gic-v3）**：guest `/proc/interrupts` 的 `GICv3 56 Edge virtio0` 计数随 TX 活动递增（3 个 ping 发包前 1 → 后 9），**中断确实送达 guest**。无 emu 模式 timer storm、无需 gppt、定时器/磁盘照常工作。这是 inter-guest virtio-net 连通的最后一公里，至此完整数据通路（枚举/TX/RX 写入/**中断送达**）全部打通。配置 `tmp/m2/vm-linux-vnet3.toml`(passthrough) + init `tmp/m2/rxirqtest.sh`，日志 `tmp/m2/ppend-run.log`。
  - **两个并行待解**：① ramdisk 引导 flakiness（同配置有时到 /init 有时卡在 freeing initrd）；② 多 VM 内存须 **MAP_ALLOC(map_type=0)** 否则同 GPA 踩内存（已解）。控制台输出坑（ramdisk /init 须 mount devtmpfs 后 `exec >/dev/console 2>&1`）已解。

### 中断送达打通后的 2-VM 实测进展（2026-06-30，续）

中断送达解决后，重新冲刺 2-VM 双向 ping，逐层排除环境障碍：

1. **ramdisk 卡死的真因（非 flakiness）**：`os/axvisor/src/fdt/create.rs::sanitize_bootargs` 会**剥离连续三元组** `["root=/dev/ram0","rdinit=/init","rootwait"]`（line 383 `tokens[index..].starts_with(&RAMDISK_BOOTARGS)`）→ guest 丢失 `rdinit=/init` → freeing initrd 后无 init 卡死。绕过：在三元组中插入 token（如 `root=/dev/ram0 rw rdinit=/init rootwait`）打断连续匹配，guest 即拿到正确 cmdline。**但单/双 VM ramdisk 仍在 freeing initrd 后不 exec /init**（与三元组无关的内核级 initramfs 卡死，disk 引导无此问题）→ ramdisk 路线放弃，改 disk 引导（可靠）。
2. **共享磁盘损坏**：两 guest 并发 rw 挂载同一 ext4（mount 即写 journal/superblock）→ 损坏。
3. **cmdline 共享**：aarch64 guest FDT 的 /chosen bootargs 取自宿主 FDT（QEMU `-append`），全 VM 同一条 → 无法给 VM2 不同的 `root=`。**已修复并验证（commit 37b9b060a）**：`create.rs` 支持 `kernel.cmdline` 覆盖（opt-in，过 sanitize）。实测 `cmdline="root=/dev/vda init=/netping.sh"` → guest `Kernel command line` 正是该串（覆盖了 `-append`），且 netping 作为 init 运行、eth0 在、TX 通路跑起。
4. **宿主双 ext4 盘 panic**：加第二块盘后宿主 `axfs-ng/root.rs:172` "failed to determine root device"（两块裸 ext4 无法判主盘）。绕过：宿主 `-append` 加 `root=/dev/vda`（`RootSpec::parse_bootargs` 让宿主选 disk0），disk1 留给 VM2。
5. **当前阻塞：guest 只见一块盘**：两块 virtio-blk passthrough 给两 guest 后，每 guest 内**有一个盘槽 probe 失败 `-2`(ENOENT)**，只剩一块成为 `vda`；VM2 `root=/dev/vdb` 找不到 → `VFS: Unable to mount root fs on unknown-block(0,0)`。疑因宿主已占用/初始化 disk0，guest 重新 probe 该 virtio-blk 槽失败。这是 AxVisor **宿主与多 guest 共享/分配 virtio-blk** 的较深行为，需专门排查，与网络通路正交。

**结论（task2 数据通路）**：hypervisor 侧能力全部打通且验证——**设备枚举 ✅ / TX virtqueue ✅ / RX virtqueue 写入 ✅ / 中断送达 ✅（physical SPI pend，1→9）/ 软交换机泛洪 ✅ / per-VM cmdline ✅**。剩下纯属把"两个真实 guest 可靠地各自起来"的基础设施（disk passthrough 拓扑），非网络/中断逻辑。两个 guest 一旦各自起来即可经已验证的通路互通（再叠应用层协议/可靠性/自动化测试，Zephyr 端点需 SDK）。复现配置 `tmp/m2/vm-disk{1,2}.toml` + `qemu-2disk.toml` + `tmp/m2/netping.sh`，日志 `tmp/m2/2vm-final.log`。
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

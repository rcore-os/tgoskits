---
sidebar_position: 11
sidebar_label: "迁移记录"
---

# 驱动框架迁移记录

宿主物理设备已经从旧 `ax-driver` 全局容器模型迁移到 `rdrive + rdif` 驱动框架。迁移围绕设备发现、能力接口、领域服务和上层消费路径分阶段完成。

## 迁移阶段

### Phase 1: rdrive backend 分发

- 增加 `PlatformSource::{Static,Fdt,Acpi}` 和 `ProbeKind::{Static,Fdt,Acpi,Pci}`。
- 新增 `probe::acpi` 模块；ACPI 初始化提供 MCFG、GSI controller routing、PCI `_PRT` 和普通设备 IRQ metadata。
- `probe_pre_kernel()` 和 `probe_all()` 改为 backend 分发，保留当前 FDT 与 PCI 能力。
- `Manager` 保持只管理 register 和 typed device registry。

### Phase 2: 补齐 rdif-display/input/vsock

- 新增三个 interface crate 并接入 workspace。
- 每个 crate 按 `error/types/interface` 或 `addr/event/interface` 拆文件。
- 不依赖 `ax-driver`、`ax-runtime`、`ax-hal` 或平台 crate。

### Phase 3: block volume service

- 抽出唯一分区扫描实现，支持 GPT、MBR、raw disk。
- 产出 `BlockVolume` 和裁剪后的 block reader。
- `ax-fs` / `ax-fs-ng` 只消费 volume 和 FS block trait。

### Phase 4: 统一网络运行时

旧 `AxNetDevice` 交付路径已替换为 `PlatformNetDevice` 的一次性移交。当前统一使用 `ax-net`，`rd-net` 是 DMA 准备和队列包装层，不是独立协议栈或 HAL IRQ 运行时。

- `NetDevice::into_parts()` 交付控制端点、队列轮询组、可选 owner startup 和硬中断端点。
- `ax-runtime::collect_net_devices()` 枚举 `rdrive`，准备 DMA 并解析完整 IRQ source 映射。
- `NetworkRuntimeBuilder` 建立固定 CPU 队列执行器和 `EthernetFramePort`，`PinnedNetIrqRegistrar` 提供平台注册边界。
- DHCP/static IP、路由和 DNS 保留在 `ax-net` 控制面，唯一协议执行器推进 smoltcp。

运行期通过 SPSC 管道移动 DMA 令牌，不再锁住完整网卡对象调用协议收发；启动和停止的所有权约束由[网络驱动](network.md)定义。

### Phase 5: display / input / vsock 硬切

- 新增 runtime wrapper `rd-display`、`rd-input`、`rd-vsock`。
- 上层 display/input/vsock 模块消费领域 service，不接收 `AxDeviceContainer`。

### Phase 6: ax-runtime 切主线

- 删除宿主初始化主线中的 `ax-driver::init_drivers()` 和 `AllDevices` 拆包。
- 平台 later init 后调用 `rdrive::probe_all(false)`。
- 调用领域 service 初始化 FS、NET、display、input、vsock。

### Phase 7: feature 映射切换

- `ax-runtime` 中旧 `ax-driver/virtio-*`、`driver-*`、`bus-*` 映射到 rdrive probe feature。
- legacy `ax-driver` feature 只保留给未迁移代码，不作为新宿主路径入口。

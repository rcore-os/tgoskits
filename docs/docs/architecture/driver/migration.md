---
sidebar_position: 11
sidebar_label: "迁移与验收"
---

# 迁移与验收

本文记录从旧 `ax-driver` 全局容器模型到 `rdrive + rdif` 驱动框架的分阶段硬切实施计划和验收标准。该迁移对应 #606 的宿主物理设备重构目标。

## 分阶段硬切实施

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

### Phase 4: NET / NET-NG 硬切

- `ax-net` / `ax-net` 从 `AxNetDevice` 切到 `rd-net` 或 net service。
- DHCP/static IP policy 留在 net service 或 NET/NET-NG，不回到 platform glue。

### Phase 5: display / input / vsock 硬切

- 新增 runtime wrapper `rd-display`、`rd-input`、`rd-vsock`。
- 上层 display/input/vsock 模块消费领域 service，不接收 `AxDeviceContainer`。

### Phase 6: ax-runtime 切主线

- 删除宿主初始化主线中的 `ax-driver::init_drivers()` 和 `AllDevices` 拆包。
- 平台 later init 后调用 `rdrive::probe_all(false)`。
- 调用领域 service 初始化 FS、NET、display、input、vsock。

### Phase 7: feature 映射切换

- `ax-feat` 中旧 `ax-driver/virtio-*`、`driver-*`、`bus-*` 映射到 rdrive probe feature。
- legacy `ax-driver` feature 只保留给未迁移代码，不作为新宿主路径入口。

## 验收标准

### 文档验收

```bash
git diff --check
cd docs
yarn build
```

本地 `docs` 未安装依赖时，先执行 `corepack enable` 与 `yarn install --frozen-lockfile`，再运行 `yarn build`。

### 代码验收

```bash
cargo xtask clippy --package rdrive
cargo xtask clippy --package ax-runtime
cargo xtask clippy --package ax-fs-ng
cargo xtask clippy --package ax-net
cargo xtask clippy --package starry-kernel
cargo xtask clippy --package axvisor
```

### 搜索验收

```bash
rg "AllDevices|AxDeviceContainer|AxBlockDevice|AxNetDevice|ax_driver::scan_partitions" os/arceos/modules os/StarryOS/kernel os/axvisor
rg "rdrive::get_|rdrive::get_one|rdrive::get_list" os/arceos/modules os/StarryOS/kernel os/axvisor
```

第二条搜索只允许 Starry USBFS 设备管理路径和 Axvisor HAL/GIC backend 出现裸 `rdrive::get_*`。

### 系统回归重点

- StarryOS QEMU smoke。
- ext4 rootfs 启动与读写。
- `net` / DHCP。
- aarch64 QEMU 动态平台配置。
- Axvisor QEMU / GIC / `rdif-intc` 路径。

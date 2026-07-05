---
sidebar_position: 1
sidebar_label: "概览"
---

# 驱动框架概览

TGOSKits 的宿主物理设备能力收敛在 `rdrive + rdif` 驱动框架。它向上为 ArceOS、StarryOS、Axvisor 提供统一的设备发现、注册、查询和领域能力接口，向下通过分层结构适配真实硬件。`rdrive` 负责设备探测（probe）、驱动注册（register）和类型化设备查询；`rdif-*` 负责各设备类别的能力边界（capability boundary）；具体硬件驱动 core 保持 `no_std` 且不耦合 OS runtime。

旧的 `ax-driver` 全局容器模型（`AllDevices`、`AxDeviceContainer`、`Ax*Device`）已移除。宿主物理设备初始化与交付主线不再经过 legacy driver crates；块设备路径的 runtime 已删除，统一以 `rdif-block` 作为 block capability boundary。`ax-driver` 现在作为共享驱动聚合 crate 接入 `rdrive + rdif`，负责 OS glue（probe、iomap、IRQ 注册、DMA 适配）。

## 源码

驱动相关源码分布在 `drivers/` 下，按职责分层组织：

| 目录 | 角色 | 关键内容 |
| --- | --- | --- |
| `drivers/rdrive/` | 设备管理框架 | `Manager`、`DriverRegister`、`PlatformDevice`、`probe::*` |
| `drivers/rdrive-macros/` | 注册宏 | `module_driver!`、linker section 收集 |
| `drivers/interface/rdif-*/` | 能力边界 | `rdif-block`、`rdif-eth`、`rdif-display`、`rdif-input`、`rdif-vsock`、`rdif-intc`、`rdif-pinctrl`、`rdif-pcie`、`rdif-clk`、`rdif-timer`、`rdif-systick`、`rdif-serial`、`rdif-pwm`、`rdif-power` |
| `drivers/ax-driver/` | OS glue / ArceOS 适配 | VirtIO、PCI、SoC、USB、serial、block/net/display/input/vsock binding |
| `drivers/blk/` | 块设备 driver core | `nvme-driver`、`sdhci-host`、`dwmmc-host`、`sdmmc-protocol`、`phytium-mci-host`、`ramdisk` |
| `drivers/net/` | 网卡 driver core | `rd-net`、`fxmac_rs`、`eth-intel`、`realtek-rtl8125` |
| `drivers/gpu/` | 显示/加速 driver core | `rockchip-rga` |
| `drivers/intc/` | 中断控制器 driver core | `arm-gic-driver`、`riscv_plic` |
| `drivers/pci/` | PCIe driver core | `pcie`、`rk3588-pci` |
| `drivers/usb/` | USB driver core | `usb-host`（CrabUSB/xHCI）、`usb-device`、`usb-serial`、`usb-if` |
| `drivers/serial/` | 串口 driver core | `some-serial` |
| `drivers/soc/` | SoC 平台 driver core | `rockchip`（clk/pinctrl/pm） |
| `drivers/tpu/`、`drivers/npu/`、`drivers/vpu/` | AI 加速 driver core | `sg2002-tpu`、`k230-kpu`、`rockchip-npu`、`rockchip-jpeg` |
| `drivers/pwm/`、`drivers/rtc/` | 平台设备 driver core | `rockchip-pwm`、`arm_pl031` |
| `drivers/firmware/` | 固件协议 | `arm-scmi-rs` |
| `drivers/examples/` | 设备树样例 | `enumerate/` |
| `drivers/data/` | 测试数据 | `qemu.dtb` / `qemu.dts` |
| `drivers/test_crates/` | 测试 crate | `driver-tests/` |

## 能力矩阵

| 能力 | interface crate | runtime crate | 上层消费 | 状态 |
| --- | --- | --- | --- | --- |
| 块设备 | `rdif-block` | 已删除，直接消费 submit/poll 边界 | block volume service、FS | 完整 |
| 网络设备 | `rdif-eth` | `rd-net` | net interface service、ax-net | 完整 |
| 显示 | `rdif-display` | `rd-display` | display service、Starry fb | 完整 |
| 输入 | `rdif-input` | `rd-input` | input service、Starry input | 完整 |
| vsock | `rdif-vsock` | `rd-vsock` | vsock service、ax-net vsock | 完整 |
| 中断控制器 | `rdif-intc` | 按需 | HAL、Axvisor GIC backend | 完整 |
| pinctrl/GPIO | `rdif-pinctrl` | 按需 | HAL、SoC glue | 完整 |
| PCIe | `rdif-pcie` | 按需 | PCI endpoint 枚举 | 完整 |
| 时钟 | `rdif-clk` | 按需 | HAL、SoC glue | 完整 |
| 定时器 | `rdif-timer` / `rdif-systick` | 按需 | HAL systick | 完整 |
| 串口 | `rdif-serial` | 按需 | early console、HAL | 完整 |
| PWM | `rdif-pwm` | 按需 | SoC glue | 完整 |
| 电源 | `rdif-power` | 按需 | SoC glue | 基础 |

## 设计原则

- **分层隔离**：Driver Core 只推进硬件状态机，不调用 `iomap`、IRQ 注册或任务调度；Capability Boundary 只定义能力契约；OS Glue 负责平台发现与注册；Runtime 负责上层运行时封装。
- **能力边界优先**：设备通过 `rdif-*` trait 向上暴露领域能力，上层模块不直接接触硬件寄存器、DMA、MMIO 或平台 IRQ ABI。
- **多来源发现**：Static、FDT、ACPI、PCI 是并列的平台发现来源，不存在唯一默认平台抽象。
- **类型化设备查询**：`rdrive::Manager` 只保存 `DriverRegister` 和类型化设备 registry，上层通过 `Device<T>` 弱引用按领域能力查询设备，不使用全局字符串匹配或大容器。
- **IRQ domain 化**：所有中断路径使用 `IrqId` 作为运行时注册 key，平台 IRQ namespace 解析留在平台 resolver 侧。
- **领域 service 消费**：上层业务模块通过领域 service 消费设备能力，不直接把 `rdrive` 当作全局设备篮子。

## 非目标与硬约束

本轮驱动框架只处理宿主侧物理设备，包括 ArceOS、StarryOS、Axvisor 在真实平台或 QEMU 平台上使用的块设备、网卡、中断控制器、pinctrl/GPIO、时钟、显示、输入、vsock、PCIe、USB host 等设备。

`axdevice` 与 `axdevice_base` 不纳入驱动框架范围。它们作为 Axvisor / axvm 的 guest emulated device model，不参与宿主物理设备 probe，不作为 FS、NET、display、input、vsock 的设备来源。

架构硬约束：

- 不新增长期存在的 `rdrive <-> ax-driver` 双向适配层。
- 不新增 `RDriveDeviceContainer`、`AllRDriveDevices` 这类换名后的 `AllDevices` 大容器。
- 不用一个 `KernelHal`、`PlatformSystem` 或其它大结构体包办 Static、FDT、ACPI、PCI、MMIO、DMA、IRQ、runtime。
- 不把 FDT 当作唯一或默认平台抽象；Static、FDT、ACPI 是并列平台来源。
- 不在 portable driver core 中调用 `iomap`、`ioremap`、`axklib`、`somehal`、任务调度或 IRQ 注册。
- 不用字符串拼接或 ad-hoc 匹配替代 FDT compatible、ACPI HID/CID、PCI vendor/device 的结构化匹配。
- 不在文档或代码中保留“以后补”的占位路径；ACPI 第一版必须返回明确 unsupported error。
- 除测试外，新增或重构后的单个 `.rs` 文件不超过 600 行。

后续各篇按层次展开：[总体架构](architecture.md)、[rdrive 设备管理](rdrive.md)、[设备探测与初始化](probe.md)、[能力边界 rdif](capability.md)、[IRQ 解析与注册](irq.md)、[驱动分层与 OS Glue](layering.md)、[领域服务与上层消费](services.md)、[Feature 与构建配置](features.md)、[系统集成](integration.md)、[迁移路径与验收](migration.md)。

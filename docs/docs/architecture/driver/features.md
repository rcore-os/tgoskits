---
sidebar_position: 9
sidebar_label: "构建配置"
---

# 构建配置

Feature 的职责从“选择 `ax-driver` 子模块和单个 Ax*Device 类型”调整为“选择要链接的 rdrive probe module、driver core、rdif 能力和 runtime wrapper”。构建配置通过 [axbuild](../../components/crates/axbuild.md) 把上层 app/system 的需求映射到具体的 `ax-driver` feature。

## Feature 映射

| 旧 feature 语义 | 新 feature 语义 |
| --- | --- |
| `ax-driver` | 启用宿主设备 probe 主线，即 `rdrive` |
| `virtio-blk` / `virtio-net` | 链接 VirtIO probe 和对应 `rdif-block` / `rdif-eth` 注册 |
| `virtio-gpu` | 链接 `rdif-display` / `rd-display` probe |
| `virtio-input` | 链接 `rdif-input` / `rd-input` probe |
| `virtio-socket` | 链接 `rdif-vsock` / `rd-vsock` probe |
| `driver-*` | 链接具体硬件 driver core 和 OS glue probe |
| `bus-*` | 链接总线枚举或控制器 probe，例如 PCIe |
| 动态平台 | 通过 FDT/ACPI/somehal 平台事实发现设备，不走 `ax_driver_*Ops` 回灌 |

MMIO 与 PCI feature 要分开表达，例如 `virtio-gpu-mmio` 与 `virtio-gpu-pci` 都是 probe module，而不是上层设备类型选择。

## ax-driver Feature

`drivers/ax-driver/Cargo.toml` 定义了按领域能力和具体硬件组织的 feature。基础 feature 选择能力边界和 runtime：

| feature | 启用 |
| --- | --- |
| `block` | `rdif-block`、block OS glue |
| `net` | `rd-net`、net OS glue |
| `display` | `rdif-display`、display OS glue |
| `input` | `rdif-input`、input OS glue |
| `vsock` | `rdif-vsock`、vsock OS glue |
| `pci` | PCIe controller、PCI endpoint 枚举 |
| `pinctrl` | `rdif-pinctrl` |
| `irq` | IRQ binding resolver |
| `serial` | `rdif-serial`、`some-serial` |

VirtIO feature 组合能力边界和 PCI transport：

| feature | 组合 |
| --- | --- |
| `virtio-blk` | `block` + `virtio` + `pci` |
| `virtio-net` | `net` + `virtio` + `pci` |
| `virtio-gpu` | `display` + `virtio` + `pci` |
| `virtio-input` | `input` + `virtio` + `pci` |
| `virtio-socket` | `vsock` + `virtio` + `pci` |

具体硬件 feature 链接对应的 driver core：

| feature | driver core |
| --- | --- |
| `ramdisk` | `ramdisk` |
| `nvme` | `nvme-driver` |
| `rockchip-sdhci` | `sdhci-host` + `sdmmc-protocol` |
| `rockchip-dwmmc` | `dwmmc-host` + `sdmmc-protocol` |
| `phytium-mci` | `phytium-mci-host` + `sdmmc-protocol` |
| `k230-sdhci` | `sdhci-host` + `sdmmc-protocol` |
| `fxmac` | `fxmac_rs` |
| `intel-net` | `eth-intel` |
| `realtek-rtl8125` | `realtek-rtl8125` |
| `aic8800-wifi` | `aic8800` + `sdhci-cv1800` + `sdio-host` |
| `rknpu` | `rockchip-npu` |
| `rga` | `rockchip-rga` |
| `jpeg` | `rockchip-jpeg` |
| `rk3588-pcie` | `rk3588-pci` |
| `rk3588-pwm` | `rockchip-pwm` + `rdif-pwm` |
| `rockchip-dwc-xhci` | `crab-usb` + Rockchip SoC |
| `xhci-mmio` / `xhci-pci` | `crab-usb` + MMIO/PCI transport |

## 配置原则

- **能力边界优先**：feature 应先表达领域能力（`block`/`net`/`display`/...），再表达具体硬件（`nvme`/`fxmac`/...）。
- **transport 分离**：MMIO 和 PCI transport 独立 feature，避免隐式耦合。
- **SoC 依赖显式**：Rockchip 等平台依赖通过 `rockchip-soc`、`rockchip-pm` 显式声明，不隐式引入。
- **feature 不选平台路径**：feature 选择链接哪些 probe module，不选择 FDT/ACPI/Static 平台来源。平台来源由 `rdrive::init(Platform::...)` 决定。

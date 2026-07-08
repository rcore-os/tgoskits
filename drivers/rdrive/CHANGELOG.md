# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.23.5](https://github.com/rcore-os/tgoskits/compare/rdrive-v0.23.4...rdrive-v0.23.5) - 2026-07-08

### Other

- updated the following local packages: ax-kspin

## [0.23.4](https://github.com/rcore-os/tgoskits/compare/rdrive-v0.23.3...rdrive-v0.23.4) - 2026-07-07

### Added

- *(rdrive)* apply assigned clocks before FDT probe ([#1527](https://github.com/rcore-os/tgoskits/pull/1527))
- *(starfive-jh7110-dwmmc)* add IRQ-driven host ([#1524](https://github.com/rcore-os/tgoskits/pull/1524))
- *(rdrive)* add FDT power-domain probing ([#1515](https://github.com/rcore-os/tgoskits/pull/1515))

### Fixed

- *(rdrive)* use preempt-safe registry locks ([#1510](https://github.com/rcore-os/tgoskits/pull/1510))

### Other

- *(drivers)* split Rockchip reset capability ([#1509](https://github.com/rcore-os/tgoskits/pull/1509))

## [0.23.3](https://github.com/rcore-os/tgoskits/compare/rdrive-v0.23.2...rdrive-v0.23.3) - 2026-07-02

### Added

- *(axvisor)* support LoongArch Linux guest on QEMU ([#1207](https://github.com/rcore-os/tgoskits/pull/1207))
- *(kspin)* add lockdep-aware spin rwlock ([#1397](https://github.com/rcore-os/tgoskits/pull/1397))

### Other

- *(rdrive)* apply default FDT pinctrl before probe ([#1458](https://github.com/rcore-os/tgoskits/pull/1458))

## [0.23.2](https://github.com/rcore-os/tgoskits/compare/rdrive-v0.23.1...rdrive-v0.23.2) - 2026-06-27

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))

### Other

- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.23.1](https://github.com/rcore-os/tgoskits/compare/rdrive-v0.23.0...rdrive-v0.23.1) - 2026-06-22

### Added

- *(starry)* add Wayland app case ([#1160](https://github.com/rcore-os/tgoskits/pull/1160))
- *(ax-net)* add multi-interface support with per-interface routing, DNS, and SO_BINDTODEVICE ([#1244](https://github.com/rcore-os/tgoskits/pull/1244))

## [0.23.0](https://github.com/rcore-os/tgoskits/compare/rdrive-v0.22.0...rdrive-v0.23.0) - 2026-06-12

### Added

- *(ax-driver)* add dynamic platform rtc support ([#1242](https://github.com/rcore-os/tgoskits/pull/1242))

### Fixed

- *(ci)* stabilize x86 Starry QEMU timing ([#1245](https://github.com/rcore-os/tgoskits/pull/1245))
- *(somehal)* route LoongArch ACPI GSIs through PCH-PIC

### Other

- *(irq)* carry ACPI IRQ routing metadata
- *(rdrive)* carry probe context and PCI INTx routes

## [0.22.0](https://github.com/rcore-os/tgoskits/compare/rdrive-v0.21.0...rdrive-v0.22.0) - 2026-06-09

### Added

- *(somehal)* register x86 ACPI IOAPIC through rdrive ([#1155](https://github.com/rcore-os/tgoskits/pull/1155))

### Fixed

- *(ci)* switch x86_64 defaults to dynamic platform ([#1024](https://github.com/rcore-os/tgoskits/pull/1024))

## [0.21.0](https://github.com/rcore-os/tgoskits/compare/rdrive-v0.20.1...rdrive-v0.21.0) - 2026-06-03

### Added

- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))

### Fixed

- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))

### Other

- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- *(driver)* move static probes to platform-owned registration ([#937](https://github.com/rcore-os/tgoskits/pull/937))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))

## [0.20.0](https://github.com/drivercraft/sparreal-os/compare/rdrive-v0.19.1...rdrive-v0.20.0) - 2026-03-10

### Other

- ✨ feat: 更新 fdt-edit 和 fdt-raw 版本，优化 FDT 相关功能 ([#47](https://github.com/drivercraft/sparreal-os/pull/47))
- ♻️ refactor(PCIe): PCIe driver use mmio_api for memory-mapped I/O operations

## [0.19.1](https://github.com/drivercraft/sparreal-os/compare/rdrive-v0.19.0...rdrive-v0.19.1) - 2026-03-06

### Other

- 🛠️ fix: 更新 secondary_entry 函数以传递 cpu_meta 参数 ([#42](https://github.com/drivercraft/sparreal-os/pull/42))

## [0.19.0](https://github.com/drivercraft/sparreal-os/compare/rdrive-v0.18.11...rdrive-v0.19.0) - 2026-03-05

### Other

- Dev/drv ([#32](https://github.com/drivercraft/sparreal-os/pull/32))
- ✨ feat: 重构设备驱动接口，移除 open/close 方法，添加 name 方法 ([#25](https://github.com/drivercraft/sparreal-os/pull/25))
- ✨ feat: 增加 rdrive ([#24](https://github.com/drivercraft/sparreal-os/pull/24))

## [0.18.11](https://github.com/drivercraft/rdrive/compare/rdrive-v0.18.10...rdrive-v0.18.11) - 2025-10-16

### Other

- 更新时钟驱动实现，替换 UART 驱动并调整探测函数
- serial

## [0.18.7](https://github.com/drivercraft/rdrive/compare/rdrive-v0.18.4...rdrive-v0.18.7) - 2025-09-25

### Fixed

- remove unused dependency enum_dispatch from Cargo.toml

## [0.18.4](https://github.com/drivercraft/rdrive/compare/rdrive-v0.18.3...rdrive-v0.18.4) - 2025-09-25

### Other

- add pcie

## [0.16.0](https://github.com/drivercraft/rdrive/compare/rdrive-v0.15.2...rdrive-v0.16.0) - 2025-06-27

### Added

- 添加 fdt_phandle_to_device_id 方法并在示例中使用

### Other

- 简化 fdt_phandle_to_device_id 函数中的模式匹配
- Merge branch 'main' of github.com:drivercraft/rdrive

## [0.15.2](https://github.com/drivercraft/rdrive/compare/rdrive-v0.15.1...rdrive-v0.15.2) - 2025-06-26

### Other

- 修改 force_use 方法，简化返回值类型
- Merge branch 'main' of github.com:drivercraft/rdrive
- 更新示例链接，指向 GitHub 上的具体实现

## [0.15.1](https://github.com/drivercraft/rdrive/compare/rdrive-v0.15.0...rdrive-v0.15.1) - 2025-06-26

### Other

- 更新 README.md，添加架构概述和驱动注册示例

## [0.15.0](https://github.com/drivercraft/rdrive/compare/rdrive-v0.14.3...rdrive-v0.15.0) - 2025-06-25

### Added

- add OnProbeError type and refactor probe functions to use it

### Other

- Merge branch 'main' of github.com:drivercraft/rdrive

## [0.14.3](https://github.com/drivercraft/rdrive/compare/rdrive-v0.14.2...rdrive-v0.14.3) - 2025-06-25

### Added

- implement Send and Sync traits for Device struct

### Other

- simplify device locking and retrieval methods

## [0.14.2](https://github.com/drivercraft/rdrive/compare/rdrive-v0.14.1...rdrive-v0.14.2) - 2025-06-25

### Added

- add rdif-net package and implement Interface trait

### Other

- Merge branch 'main' of github.com:drivercraft/rdrive

## [0.14.1](https://github.com/drivercraft/rdrive/compare/rdrive-v0.14.0...rdrive-v0.14.1) - 2025-06-24

### Other

- update driver macros to use AsAny for type downcasting

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.13](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.12...axplat-dyn-v0.7.13) - 2026-07-23

### Other

- *(cpu-local)* extract per-CPU register ownership ([#1662](https://github.com/rcore-os/tgoskits/pull/1662))

### Changed

- *(cpu-local)* validate and install exact `CpuAreaRef` values from someboot's frozen dynamic
  CPU-area layout without version/generation/cookie fields, a base callback, or linked-layout
  feature propagation.

## [0.7.12](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.11...axplat-dyn-v0.7.12) - 2026-07-10

### Added

- *(msi)* add hierarchical MSI-X irq domains ([#1526](https://github.com/rcore-os/tgoskits/pull/1526))

## [0.7.11](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.10...axplat-dyn-v0.7.11) - 2026-07-08

### Fixed

- *(platforms)* route DMA cache sync through platform cache ops ([#1542](https://github.com/rcore-os/tgoskits/pull/1542))

## [0.7.10](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.9...axplat-dyn-v0.7.10) - 2026-07-08

### Other

- updated the following local packages: ax-cpu, ax-plat, someboot, axklib, ax-driver, somehal

## [0.7.9](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.8...axplat-dyn-v0.7.9) - 2026-07-08

### Other

- updated the following local packages: rdrive, ax-plat, axklib, ax-driver, somehal

## [0.7.8](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.7...axplat-dyn-v0.7.8) - 2026-07-07

### Fixed

- *(ci)* restore Starry ptrace and Axvisor RISC-V tests ([#1521](https://github.com/rcore-os/tgoskits/pull/1521))

### Other

- remove static platform and axconfig generation, make dynamic platform the only path ([#1478](https://github.com/rcore-os/tgoskits/pull/1478))

## [0.7.7](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.6...axplat-dyn-v0.7.7) - 2026-07-02

### Added

- *(somehal)* allocate interrupt controller domains
- *(axvisor)* support LoongArch Linux guest on QEMU ([#1207](https://github.com/rcore-os/tgoskits/pull/1207))

### Fixed

- *(ax-hal)* route typed IPI ids through platform irq
- *(irq)* avoid hard irq controller locks
- *(irq)* close domain runtime review gaps

### Other

- *(somehal)* modernize x86 qemu irq routing ([#1430](https://github.com/rcore-os/tgoskits/pull/1430))

## [0.7.6](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.5...axplat-dyn-v0.7.6) - 2026-06-27

### Added

- *(ax-runtime)* generate banner build info ([#1373](https://github.com/rcore-os/tgoskits/pull/1373))

### Other

- *(platform)* remove ax-config from dynamic runtime path ([#1387](https://github.com/rcore-os/tgoskits/pull/1387))
- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.7.5](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.4...axplat-dyn-v0.7.5) - 2026-06-23

### Added

- *(starry)* support reboot syscall ([#1358](https://github.com/rcore-os/tgoskits/pull/1358))

### Fixed

- *(platform)* support AArch64 HVF timer boot ([#1334](https://github.com/rcore-os/tgoskits/pull/1334))

## [0.7.4](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.3...axplat-dyn-v0.7.4) - 2026-06-22

### Added

- *(ax-runtime)* prefer UEFI RTC on dynamic platform ([#1294](https://github.com/rcore-os/tgoskits/pull/1294))

## [0.7.3](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.2...axplat-dyn-v0.7.3) - 2026-06-12

### Fixed

- *(ci)* stabilize x86 Starry QEMU timing ([#1245](https://github.com/rcore-os/tgoskits/pull/1245))

## [0.7.2](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.1...axplat-dyn-v0.7.2) - 2026-06-11

### Added

- *(somehal)* support dynamic CPU and interrupt hooks

## [0.7.1](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.7.0...axplat-dyn-v0.7.1) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))

### Fixed

- *(ci)* switch x86_64 defaults to dynamic platform ([#1024](https://github.com/rcore-os/tgoskits/pull/1024))

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.6.2...axplat-dyn-v0.7.0) - 2026-06-03

### Added

- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))

### Other

- *(linker)* layer platform runtime and final scripts ([#1075](https://github.com/rcore-os/tgoskits/pull/1075))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.6.1...axplat-dyn-v0.6.2) - 2026-05-22

### Added

- *(drivers)* add SD/MMC platform driver support ([#826](https://github.com/rcore-os/tgoskits/pull/826))

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.6.0...axplat-dyn-v0.6.1) - 2026-05-19

### Other

- updated the following local packages: ax-arm-pl031, ax-errno, ax-driver-virtio, ax-alloc, ax-cpu, ax-driver-net, axklib

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.5.12...axplat-dyn-v0.6.0) - 2026-05-15

### Added

- *(drivers)* migrate Sparreal driver crates ([#540](https://github.com/rcore-os/tgoskits/pull/540))
- *(somehal)* Add initial implementation of SomeHAL for hardware abstraction
- *(axplat-dyn)* add RK3588 USB board support
- *(axplat-dyn)* add USB host integration
- *(realtek-rtl8125)* complete OrangePi board bringup ([#404](https://github.com/rcore-os/tgoskits/pull/404))
- *(ax-task)* add stack canary checks for multitask stacks ([#416](https://github.com/rcore-os/tgoskits/pull/416))
- *(axplat-dyn)* add RK3588 PCIe host support ([#396](https://github.com/rcore-os/tgoskits/pull/396))
- *(runtime)* extend IRQ, RTC, and tty event support ([#287](https://github.com/rcore-os/tgoskits/pull/287))
- *(rockchip-soc)* migrate RK3588 clocks ([#384](https://github.com/rcore-os/tgoskits/pull/384))
- *(console)* add interrupt-driven console input ([#343](https://github.com/rcore-os/tgoskits/pull/343))

### Fixed

- *(starry-kernel)* repair serial console input on dynamic platforms ([#555](https://github.com/rcore-os/tgoskits/pull/555))
- *(rockchip-soc)* enable RK3588 USB PHY clocks ([#528](https://github.com/rcore-os/tgoskits/pull/528))
- *(axplat-dyn)* tolerate unavailable Rockchip SD/MMC devices ([#434](https://github.com/rcore-os/tgoskits/pull/434))
- *(console)* keep UART writes raw ([#402](https://github.com/rcore-os/tgoskits/pull/402))
- update kernel entry points for FreeRTOS and Zephyr VM configuratons ([#390](https://github.com/rcore-os/tgoskits/pull/390))

### Other

- Adds a StarryOS YOLOv8 UVC camera demo for OrangePi 5 Plus with RKNN/NPU inference and HTTP MJPEG streaming. ([#574](https://github.com/rcore-os/tgoskits/pull/574))
- 增强 ArceOS 中 VirtIO Net、Vsock 及通用探测路径 ([#376](https://github.com/rcore-os/tgoskits/pull/376))
- Merge pull request #397 from rcore-os/yanlien/dev
- *(platform)* inherit workspace metadata

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/axplat-dyn-v0.5.11...axplat-dyn-v0.5.12) - 2026-04-27

### Other

- Implement RK3588 CRU driver with NPU support and enhancements ([#241](https://github.com/rcore-os/tgoskits/pull/241))

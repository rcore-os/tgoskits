# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.2](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.8.1...ax-driver-v0.8.2) - 2026-06-23

### Other

- updated the following local packages: rd-net, aic8800, ax-kspin, dma-api, ax-alloc, axklib, some-serial, crab-usb, dwmmc-host, eth-intel, rdif-block, nvme-driver, phytium-mci-host, ramdisk, realtek-rtl8125, rockchip-npu, rockchip-rga, rockchip-soc, sdhci-host

## [0.8.1](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.8.0...ax-driver-v0.8.1) - 2026-06-22

### Added

- *(starry)* add Wayland app case ([#1160](https://github.com/rcore-os/tgoskits/pull/1160))
- *(rockchip-rga)* add dry-run command buffer ([#1248](https://github.com/rcore-os/tgoskits/pull/1248))
- AIC8800 Wi-Fi SoftAP for SG2002 (LicheeRV Nano) ([#1185](https://github.com/rcore-os/tgoskits/pull/1185))

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.7.1...ax-driver-v0.8.0) - 2026-06-12

### Added

- *(ax-driver)* add dynamic platform rtc support ([#1242](https://github.com/rcore-os/tgoskits/pull/1242))

### Fixed

- *(somehal)* route LoongArch ACPI GSIs through PCH-PIC

### Other

- *(irq)* carry ACPI IRQ routing metadata
- *(ax-driver)* normalize FDT PCI IRQ source resolution
- *(ax-driver)* register devices with binding info
- *(rdrive)* carry probe context and PCI INTx routes

## [0.7.1](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.7.0...ax-driver-v0.7.1) - 2026-06-11

### Fixed

- *(kernel)* harden early allocation and virtio PCI setup

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.6.1...ax-driver-v0.7.0) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))
- *(somehal)* register x86 ACPI IOAPIC through rdrive ([#1155](https://github.com/rcore-os/tgoskits/pull/1155))

### Fixed

- *(ci)* switch x86_64 defaults to dynamic platform ([#1024](https://github.com/rcore-os/tgoskits/pull/1024))

### Other

- *(ax-driver)* remove redundant mmio cfg gate ([#1100](https://github.com/rcore-os/tgoskits/pull/1100))

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.6.0...ax-driver-v0.6.1) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))
- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(starryos)* add QEMU K230 boot support ([#1046](https://github.com/rcore-os/tgoskits/pull/1046))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))
- *(some-serial)* add Rockchip FIQ debugger UART ([#980](https://github.com/rcore-os/tgoskits/pull/980))

### Other

- *(platform)* migrate riscv64 qemu to dynamic platform ([#1085](https://github.com/rcore-os/tgoskits/pull/1085))
- *(platform)* remove static aarch64 platforms ([#1074](https://github.com/rcore-os/tgoskits/pull/1074))
- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.5.14...ax-driver-v0.6.0) - 2026-05-19

### Fixed

- *(starry)* weston bringup fixes + IRQ wakers + AF_UNIX cmsg byte marks ([#509](https://github.com/rcore-os/tgoskits/pull/509))

### Other

- Refactor Clippy integration and enhance package handling ([#738](https://github.com/rcore-os/tgoskits/pull/738))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.5.13...ax-driver-v0.5.14) - 2026-05-15

### Other

- updated the following local packages: ax-driver-input, ax-driver-virtio, ax-alloc, ax-config, axplat-dyn, ax-hal, ax-driver-net, ax-dma

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.5.11...ax-driver-v0.5.12) - 2026-04-27

### Other

- updated the following local packages: ax-alloc

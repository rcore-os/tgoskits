# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.2](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.7.1...nvme-driver-v0.7.2) - 2026-07-10

### Added

- *(msi)* add hierarchical MSI-X irq domains ([#1526](https://github.com/rcore-os/tgoskits/pull/1526))

## [0.7.1](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.7.0...nvme-driver-v0.7.1) - 2026-07-08

### Other

- updated the following local packages: dma-api, rdif-block

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.6.3...nvme-driver-v0.7.0) - 2026-07-07

### Added

- *(msi)* add aarch64 MSI-X registration ([#1522](https://github.com/rcore-os/tgoskits/pull/1522))

## [0.6.3](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.6.2...nvme-driver-v0.6.3) - 2026-07-02

### Fixed

- *(ci)* prevent Starry qemu hangs in IRQ paths ([#1431](https://github.com/rcore-os/tgoskits/pull/1431))

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.6.1...nvme-driver-v0.6.2) - 2026-06-27

### Added

- *(rdif-block)* add owned DMA queue primitives

### Other

- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.6.0...nvme-driver-v0.6.1) - 2026-06-23

### Other

- updated the following local packages: dma-api, rdif-block

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.5.2...nvme-driver-v0.6.0) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.5.2](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.5.1...nvme-driver-v0.5.2) - 2026-06-12

### Other

- updated the following local packages: rdif-block

## [0.5.1](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.5.0...nvme-driver-v0.5.1) - 2026-06-09

### Other

- updated the following local packages: pcie, dma-api, rdif-block

## [0.5.0](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.4.2...nvme-driver-v0.5.0) - 2026-06-03

### Added

- *(axbuild)* support Starry QEMU apps ([#1078](https://github.com/rcore-os/tgoskits/pull/1078))
- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))

### Fixed

- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))

### Other

- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(dma-api)* split coherent and streaming DMA APIs ([#932](https://github.com/rcore-os/tgoskits/pull/932))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))

## [0.4.1](https://github.com/drivercraft/sparreal-os/compare/nvme-driver-v0.4.0...nvme-driver-v0.4.1) - 2026-03-10

### Other

- ✨ feat: 更新 fdt-edit 和 fdt-raw 版本，优化 FDT 相关功能 ([#47](https://github.com/drivercraft/sparreal-os/pull/47))
- ♻️ refactor(PCIe): PCIe driver use mmio_api for memory-mapped I/O operations

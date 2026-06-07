# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

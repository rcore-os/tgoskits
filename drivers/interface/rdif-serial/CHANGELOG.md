# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.9.0](https://github.com/rcore-os/tgoskits/compare/rdif-serial-v0.8.2...rdif-serial-v0.9.0) - 2026-06-27

### Other

- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.8.2](https://github.com/rcore-os/tgoskits/compare/rdif-serial-v0.8.1...rdif-serial-v0.8.2) - 2026-06-23

### Other

- *(repo)* add focused unit tests for panic and serial helpers ([#1304](https://github.com/rcore-os/tgoskits/pull/1304))

## [0.8.1](https://github.com/rcore-os/tgoskits/compare/rdif-serial-v0.8.0...rdif-serial-v0.8.1) - 2026-06-12

### Other

- updated the following local packages: rdif-base

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/rdif-serial-v0.7.3...rdif-serial-v0.8.0) - 2026-06-11

### Fixed

- fix typos in code and comments across the codebase ([#1206](https://github.com/rcore-os/tgoskits/pull/1206))

## [0.7.3](https://github.com/rcore-os/tgoskits/compare/rdif-serial-v0.7.2...rdif-serial-v0.7.3) - 2026-06-09

### Other

- updated the following local packages: rdif-base

## [0.7.2](https://github.com/rcore-os/tgoskits/compare/rdif-serial-v0.7.1...rdif-serial-v0.7.2) - 2026-06-03

### Added

- *(some-serial)* add Rockchip FIQ debugger UART ([#980](https://github.com/rcore-os/tgoskits/pull/980))

### Fixed

- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))

### Other

- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))

## [0.5.3](https://github.com/drivercraft/rdrive/compare/rdif-serial-v0.5.2...rdif-serial-v0.5.3) - 2025-10-16

### Fixed

- 修改 Register trait 中 read_byte 方法的可变性，优化 Serial 结构体的内存管理

### Other

- 更新 TransferError 枚举，移除 RegisterTransferError，简化错误处理逻辑

## [0.5.2](https://github.com/drivercraft/rdrive/compare/rdif-serial-v0.5.1...rdif-serial-v0.5.2) - 2025-10-16

### Other

- Merge branch 'main' of github.com:drivercraft/rdrive
- 优化代码格式和结构，调整导入顺序，简化内存管理逻辑

## [0.5.1](https://github.com/drivercraft/rdrive/compare/rdif-serial-v0.5.0...rdif-serial-v0.5.1) - 2025-10-16

### Other

- *(rdrive)* release v0.18.11 ([#26](https://github.com/drivercraft/rdrive/pull/26))

## [0.4.2](https://github.com/drivercraft/rdrive/compare/rdif-serial-v0.4.1...rdif-serial-v0.4.2) - 2025-09-25

### Other

- enum

## [0.4.1](https://github.com/drivercraft/rdrive/compare/rdif-serial-v0.4.0...rdif-serial-v0.4.1) - 2025-09-23

### Fixed

- fix test

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.9.3](https://github.com/rcore-os/tgoskits/compare/crab-usb-v0.9.1...crab-usb-v0.9.3) - 2026-05-18

### Added

- *(dma-api)* vendor dma and mmio api crates ([#742](https://github.com/rcore-os/tgoskits/pull/742))

### Fixed

- *(ci)* address usb release and axaddrspace std failures ([#743](https://github.com/rcore-os/tgoskits/pull/743))

### Other

- bump up usb version
- Refactor USB driver configuration and tests
- *(drivers)* integrate usb crates into workspace

## [0.9.1](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.9.0...crab-usb-v0.9.1) - 2026-05-09

### Other

- *(crab-usb)* change queue logic ([#79](https://github.com/drivercraft/CrabUSB/pull/79))

## [0.9.0](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.8.2...crab-usb-v0.9.0) - 2026-05-08

### Fixed

- improve xHCI CAM transfer handling ([#77](https://github.com/drivercraft/CrabUSB/pull/77))

## [0.8.2](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.8.1...crab-usb-v0.8.2) - 2026-05-07

### Fixed

- *(ring)* adjust TRB length calculation to support multiple ring pages ([#75](https://github.com/drivercraft/CrabUSB/pull/75))

## [0.8.1](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.8.0...crab-usb-v0.8.1) - 2026-04-30

### Fixed

- *(kmod)* propagate DMA mapping errors and pre-check ring capacity ([#73](https://github.com/drivercraft/CrabUSB/pull/73))

## [0.8.0](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.7.1...crab-usb-v0.8.0) - 2026-04-30

### Other

- Introduce Queue-Based Transfer API and Unified Endpoint Model ([#71](https://github.com/drivercraft/CrabUSB/pull/71))

## [0.7.1](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.7.0...crab-usb-v0.7.1) - 2026-04-29

### Fixed

- *(xhci)* set chain bit on ISO TRBs and preserve raw config descriptor ([#69](https://github.com/drivercraft/CrabUSB/pull/69))

## [0.7.0](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.6.2...crab-usb-v0.7.0) - 2026-04-23

### Other

- [codex] support variable packet lengths for isochronous transfers ([#64](https://github.com/drivercraft/CrabUSB/pull/64))

## [0.6.2](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.6.1...crab-usb-v0.6.2) - 2026-01-28

### Other

- ♻️ refactor(device): update hub speed assignment logic and clean up comments
- ♻️ refactor(device): rename input_clean_change to perper_change for clarity

## [0.6.1](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.6.0...crab-usb-v0.6.1) - 2026-01-28

### Other

- ♻️ refactor(hub): remove unused RouteString and clean up HubParams structure
- ♻️ refactor(hub): enhance HubInfo structure and update initialization logic

## [0.6.0](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.5.0...crab-usb-v0.6.0) - 2026-01-27

### Added

- update DMA API usage and improve alignment handling in transfer operations
- *(transfer)* make Transfer::new_in and Transfer::new_out methods crate-private and improve DMA mapping logic
- *(endpoint)* 根据 xHCI 规范调整传输长度计算逻辑 ([#49](https://github.com/drivercraft/CrabUSB/pull/49))

### Other

- ♻️ refactor(uvc): clean up code and improve cargo aliases ([#58](https://github.com/drivercraft/CrabUSB/pull/58))
- ♻️ refactor(libusb): remove unused extra field extraction from InterfaceDescriptor ([#57](https://github.com/drivercraft/CrabUSB/pull/57))
- simplify route string handling in Device::address method
- clean up Cargo.toml dependencies and remove unused constants in DWC3
- improve error handling in VideoStream and update endpoint imports
- add queue module and update module visibility in backend
- clean up imports and improve error handling in USB backend
- update transfer length check and add cache invalidation comment in Endpoint
- update import paths and improve transfer handling in USB backend
- Refactor USB Host Backend and Implement LibUSB Support
- update DMA API version and improve memory handling in device descriptor
- update DMA API usage and improve memory allocation logic across modules
- update DMA API path and improve coherent memory allocation logic
- update error handling to use anyhow for better error context and simplify imports across modules
- rename DeviceSpeed to Speed for consistency across modules
- update DeviceSpeed enum and simplify its usage across modules
- remove unused DeviceDma imports and simplify code structure
- Refactor USB host crate dependencies and features
- *(transfer)* update mapping to use Option and simplify DMA handling
- Refactor USB host backend to use Kernel abstraction for DMA operations
- Refactor USB host driver to integrate DMA API
- ✨ feat(xhci): add DMA address range validation for transfers
- 移除 dma_mask 参数并根据控制器能力自动调整
- 删除 usb-host/bare-test.toml 配置文件
- 移除多余的 Multi-TT 支持字段
- [fix] hub on real world works ([#52](https://github.com/drivercraft/CrabUSB/pull/52))
- [feat] add hub support ([#51](https://github.com/drivercraft/CrabUSB/pull/51))

## [0.5.0](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.3.10...crab-usb-v0.5.0) - 2025-11-19

### Other

- Update ostool ([#37](https://github.com/drivercraft/CrabUSB/pull/37))

## [0.3.10](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.3.9...crab-usb-v0.3.10) - 2025-09-03

### Other

- 清理未使用的导入和注释掉的代码

## [0.3.9](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.3.8...crab-usb-v0.3.9) - 2025-09-02

### Other

- Merge branch 'main' of github.com:drivercraft/CrabUSB
- update
- update

## [0.3.8](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.3.7...crab-usb-v0.3.8) - 2025-09-01

### Added

- 添加测试和设备支持，优化依赖项配置

### Fixed

- set_interface

### Other

- xhci init

## [0.3.7](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.3.6...crab-usb-v0.3.7) - 2025-08-29

### Other

- fmt code

## [0.3.6](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.3.5...crab-usb-v0.3.6) - 2025-08-29

### Other

- improve event handling and memory safety in USBHost and EventHandler

## [0.3.5](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.3.4...crab-usb-v0.3.5) - 2025-08-26

### Fixed

- update rust-toolchain channel to nightly and bump bare-test dependency to 0.6

## [0.3.3](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.3.2...crab-usb-v0.3.3) - 2025-08-12

### Fixed

- trait-ffi version error

## [0.3.2](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.3.1...crab-usb-v0.3.2) - 2025-08-12

### Other

- enhance error handling in libusb context and error modules ([#26](https://github.com/drivercraft/CrabUSB/pull/26))

## [0.3.0](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.2.3...crab-usb-v0.3.0) - 2025-08-09

### Fixed

- handle enabled ports with no device ([#20](https://github.com/drivercraft/CrabUSB/pull/20))

## [0.2.3](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.2.2...crab-usb-v0.2.3) - 2025-08-08

### Other

- Dev uvc ([#17](https://github.com/drivercraft/CrabUSB/pull/17))

## [0.2.2](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.2.1...crab-usb-v0.2.2) - 2025-08-07

### Fixed

- transfer ring

### Other

- Merge branch 'main' of github.com:drivercraft/CrabUSB
- keyboard

## [0.2.1](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.2.0...crab-usb-v0.2.1) - 2025-08-07

### Added

- libusb transfer ok

### Other

- update
- fmt code

## [0.2.0](https://github.com/drivercraft/CrabUSB/compare/crab-usb-v0.1.3...crab-usb-v0.2.0) - 2025-08-05

### Other

- ci ([#11](https://github.com/drivercraft/CrabUSB/pull/11))

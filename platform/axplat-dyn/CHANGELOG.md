# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

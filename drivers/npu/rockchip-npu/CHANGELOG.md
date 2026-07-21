# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.11](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.2.10...rockchip-npu-v0.2.11) - 2026-07-21

### Other

- update Cargo.toml dependencies

## [0.2.10](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.2.9...rockchip-npu-v0.2.10) - 2026-07-08

### Other

- updated the following local packages: dma-api

## [0.2.9](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.2.8...rockchip-npu-v0.2.9) - 2026-07-07

### Other

- update Cargo.toml dependencies

## [0.2.8](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.2.7...rockchip-npu-v0.2.8) - 2026-07-02

### Added

- *(rockchip-jpeg)* add RK3588 hardware JPEG decoder (VDPU720) with MPP /dev/mpp_service ([#1456](https://github.com/rcore-os/tgoskits/pull/1456))

## [0.2.7](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.2.6...rockchip-npu-v0.2.7) - 2026-06-27

### Added

- *(rdif-block)* add owned DMA queue primitives

### Fixed

- *(rknpu)* honor GEM cache flags for mmap ([#1364](https://github.com/rcore-os/tgoskits/pull/1364))

## [0.2.6](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.2.5...rockchip-npu-v0.2.6) - 2026-06-23

### Other

- updated the following local packages: dma-api

## [0.2.5](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.2.4...rockchip-npu-v0.2.5) - 2026-06-12

### Other

- updated the following local packages: rdif-base

## [0.2.4](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.2.3...rockchip-npu-v0.2.4) - 2026-06-11

### Added

- *(orangepi-5-plus-uvc-rknn)* add RKNN bench validation ([#1189](https://github.com/rcore-os/tgoskits/pull/1189))

## [0.2.3](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.2.2...rockchip-npu-v0.2.3) - 2026-06-09

### Other

- updated the following local packages: rdif-base, dma-api

## [0.2.2](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.2.1...rockchip-npu-v0.2.2) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))

### Other

- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- *(dma-api)* split coherent and streaming DMA APIs ([#932](https://github.com/rcore-os/tgoskits/pull/932))

## [0.2.1](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.2.0...rockchip-npu-v0.2.1) - 2026-05-22

### Other

- updated the following local packages: rockchip-soc

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.1.1...rockchip-npu-v0.2.0) - 2026-05-15

### Added

- *(drivers)* migrate Sparreal driver crates ([#540](https://github.com/rcore-os/tgoskits/pull/540))
- *(rockchip-soc)* migrate RK3588 clocks ([#384](https://github.com/rcore-os/tgoskits/pull/384))

### Other

- Adds a StarryOS YOLOv8 UVC camera demo for OrangePi 5 Plus with RKNN/NPU inference and HTTP MJPEG streaming. ([#574](https://github.com/rcore-os/tgoskits/pull/574))
- remove unused dependencies and clean up test code ([#392](https://github.com/rcore-os/tgoskits/pull/392))
- *(rockchip-npu)* inherit workspace metadata

## [0.1.1](https://github.com/rcore-os/tgoskits/compare/rockchip-npu-v0.1.0...rockchip-npu-v0.1.1) - 2026-04-27

### Other

- update Cargo.toml dependencies

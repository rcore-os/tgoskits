# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.7](https://github.com/rcore-os/tgoskits/compare/realtek-rtl8125-v0.2.6...realtek-rtl8125-v0.2.7) - 2026-07-01

### Other

- *(net)* split IRQ handlers from NIC queues ([#1435](https://github.com/rcore-os/tgoskits/pull/1435))

## [0.2.6](https://github.com/rcore-os/tgoskits/compare/realtek-rtl8125-v0.2.5...realtek-rtl8125-v0.2.6) - 2026-06-27

### Added

- *(rdif-block)* add owned DMA queue primitives

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))

## [0.2.5](https://github.com/rcore-os/tgoskits/compare/realtek-rtl8125-v0.2.4...realtek-rtl8125-v0.2.5) - 2026-06-23

### Other

- updated the following local packages: rdif-eth, dma-api

## [0.2.4](https://github.com/rcore-os/tgoskits/compare/realtek-rtl8125-v0.2.3...realtek-rtl8125-v0.2.4) - 2026-06-22

### Other

- updated the following local packages: rdif-eth

## [0.2.3](https://github.com/rcore-os/tgoskits/compare/realtek-rtl8125-v0.2.2...realtek-rtl8125-v0.2.3) - 2026-06-12

### Other

- updated the following local packages: rdif-eth

## [0.2.2](https://github.com/rcore-os/tgoskits/compare/realtek-rtl8125-v0.2.1...realtek-rtl8125-v0.2.2) - 2026-06-09

### Other

- updated the following local packages: dma-api, rdif-eth

## [0.2.1](https://github.com/rcore-os/tgoskits/compare/realtek-rtl8125-v0.2.0...realtek-rtl8125-v0.2.1) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))

### Fixed

- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))

### Other

- *(dma-api)* split coherent and streaming DMA APIs ([#932](https://github.com/rcore-os/tgoskits/pull/932))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/realtek-rtl8125-v0.1.0...realtek-rtl8125-v0.2.0) - 2026-05-15

### Added

- *(drivers)* migrate Sparreal driver crates ([#540](https://github.com/rcore-os/tgoskits/pull/540))

### Other

- Adds a StarryOS YOLOv8 UVC camera demo for OrangePi 5 Plus with RKNN/NPU inference and HTTP MJPEG streaming. ([#574](https://github.com/rcore-os/tgoskits/pull/574))

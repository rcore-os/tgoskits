# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.7](https://github.com/rcore-os/tgoskits/compare/rd-net-v0.1.6...rd-net-v0.1.7) - 2026-06-27

### Added

- *(rdif-block)* add owned DMA queue primitives

## [0.1.6](https://github.com/rcore-os/tgoskits/compare/rd-net-v0.1.5...rd-net-v0.1.6) - 2026-06-23

### Other

- *(ax-net)* add locking and concurrency documentation and remove deprecated interfaces ([#1340](https://github.com/rcore-os/tgoskits/pull/1340))

## [0.1.5](https://github.com/rcore-os/tgoskits/compare/rd-net-v0.1.4...rd-net-v0.1.5) - 2026-06-22

### Added

- *(starry)* add Wayland app case ([#1160](https://github.com/rcore-os/tgoskits/pull/1160))
- *(poll)* add irq-safe deferred notifications ([#1278](https://github.com/rcore-os/tgoskits/pull/1278))
- runtime Wi-Fi AP/STA mode switch for AIC8800 on SG2002 (LicheeRV Nano) ([#1266](https://github.com/rcore-os/tgoskits/pull/1266))
- AIC8800 Wi-Fi SoftAP for SG2002 (LicheeRV Nano) ([#1185](https://github.com/rcore-os/tgoskits/pull/1185))

## [0.1.4](https://github.com/rcore-os/tgoskits/compare/rd-net-v0.1.3...rd-net-v0.1.4) - 2026-06-12

### Added

- *(axruntime)* add runtime IRQ registration adapters

## [0.1.3](https://github.com/rcore-os/tgoskits/compare/rd-net-v0.1.2...rd-net-v0.1.3) - 2026-06-09

### Other

- updated the following local packages: dma-api, rdif-eth

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/rd-net-v0.1.1...rd-net-v0.1.2) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))

### Other

- *(dma-api)* split coherent and streaming DMA APIs ([#932](https://github.com/rcore-os/tgoskits/pull/932))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))

## [0.1.0](https://github.com/drivercraft/sparreal-os/releases/tag/rd-net-v0.1.0) - 2026-04-10

### Other

- ✨ feat(rd-net): 添加网络传输包装层，简化 DMA 缓冲区管理 ([#72](https://github.com/drivercraft/sparreal-os/pull/72))

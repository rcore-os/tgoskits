# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Keep CPU-owned DMA buffers pointer-sized by moving allocation metadata into
  one fallible ownership record created with the buffer, while preserving
  exact-once deallocation and cache-direction transitions.

## [0.9.3](https://github.com/rcore-os/tgoskits/compare/dma-api-v0.9.2...dma-api-v0.9.3) - 2026-07-08

### Other

- updated the following local packages: ax-kspin

## [0.9.2](https://github.com/rcore-os/tgoskits/compare/dma-api-v0.9.1...dma-api-v0.9.2) - 2026-07-07

### Other

- updated the following local packages: ax-kspin

## [0.9.1](https://github.com/rcore-os/tgoskits/compare/dma-api-v0.9.0...dma-api-v0.9.1) - 2026-07-02

### Other

- updated the following local packages: ax-kspin

## [0.9.0](https://github.com/rcore-os/tgoskits/compare/dma-api-v0.8.2...dma-api-v0.9.0) - 2026-06-27

### Added

- *(rdif-block)* add owned DMA queue primitives

## [0.8.2](https://github.com/rcore-os/tgoskits/compare/dma-api-v0.8.1...dma-api-v0.8.2) - 2026-06-23

### Other

- updated the following local packages: ax-kspin

## [0.8.1](https://github.com/rcore-os/tgoskits/compare/dma-api-v0.8.0...dma-api-v0.8.1) - 2026-06-09

### Other

- updated the following local packages: ax-kspin

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/dma-api-v0.7.3...dma-api-v0.8.0) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))

### Other

- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))

## [0.7.3](https://github.com/rcore-os/tgoskits/compare/dma-api-v0.7.2...dma-api-v0.7.3) - 2026-05-18

### Added

- *(dma-api)* vendor dma and mmio api crates ([#742](https://github.com/rcore-os/tgoskits/pull/742))

## [0.7.2](https://github.com/drivercraft/sparreal-os/compare/dma-api-v0.7.1...dma-api-v0.7.2) - 2026-04-10

### Other

- ✨ feat(rd-net): 添加网络传输包装层，简化 DMA 缓冲区管理 ([#72](https://github.com/drivercraft/sparreal-os/pull/72))

## [0.7.1](https://github.com/drivercraft/sparreal-os/compare/dma-api-v0.7.0...dma-api-v0.7.1) - 2026-03-04

### Other

- ✨ feat: 重构设备驱动接口，移除 open/close 方法，添加 name 方法 ([#25](https://github.com/drivercraft/sparreal-os/pull/25))

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.4](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.3.3...dwmmc-host-v0.3.4) - 2026-08-03

### Fixed

- *(dma-api)* retire legacy axdma release paths ([#1796](https://github.com/rcore-os/tgoskits/pull/1796))

### Other

- *(block)* adopt IRQ-driven multi-queue runtime ([#1768](https://github.com/rcore-os/tgoskits/pull/1768))

### Changed

- Replace per-request descriptor allocation and DMA reset with one reusable
  4 KiB IDMAC descriptor ring owned by the controller.
- Advance command and data requests only after acknowledged IRQ events, and
  expose physical IRQ enable/disable through `SdioIrqHost`.
- Build only the active IDMAC chain for each request and route acknowledgement
  exclusively through the owned IRQ endpoint.
- Match all shared protocol progress and bus-width states exhaustively instead
  of treating an unknown terminal state as pending or an unknown width as
  1-bit.

### Removed

- Remove FIFO block fallback, completion polling, and per-request DMA reset.
- Remove the direct host `handle_irq` compatibility entry point.

## [0.3.3](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.3.2...dwmmc-host-v0.3.3) - 2026-07-23

### Fixed

- *(dwmmc-host)* enforce 32-bit response MMIO reads ([#1647](https://github.com/rcore-os/tgoskits/pull/1647))

## [0.3.2](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.3.1...dwmmc-host-v0.3.2) - 2026-07-08

### Other

- updated the following local packages: dma-api, sdio-host2, rdif-block, sdmmc-protocol

## [0.3.1](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.3.0...dwmmc-host-v0.3.1) - 2026-07-07

### Added

- *(starfive-jh7110-dwmmc)* add IRQ-driven host ([#1524](https://github.com/rcore-os/tgoskits/pull/1524))

### Other

- *(sdmmc-protocol)* split SDIO and RDIF capability modules ([#1486](https://github.com/rcore-os/tgoskits/pull/1486))

## [0.3.0](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.2.0...dwmmc-host-v0.3.0) - 2026-07-02

### Other

- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.1.5...dwmmc-host-v0.2.0) - 2026-06-27

### Added

- *(sdmmc)* implement native host2 RDIF path

## [0.1.5](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.1.4...dwmmc-host-v0.1.5) - 2026-06-23

### Other

- updated the following local packages: dma-api

## [0.1.4](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.1.3...dwmmc-host-v0.1.4) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.1.3](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.1.2...dwmmc-host-v0.1.3) - 2026-06-09

### Other

- updated the following local packages: dma-api

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.1.1...dwmmc-host-v0.1.2) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))

### Other

- *(dma-api)* split coherent and streaming DMA APIs ([#932](https://github.com/rcore-os/tgoskits/pull/932))

## [0.1.0](https://github.com/rcore-os/tgoskits/releases/tag/dwmmc-host-v0.1.0) - 2026-05-16

### Added

- *(sdmmc)* add reusable SD/MMC protocol and host drivers ([#538](https://github.com/rcore-os/tgoskits/pull/538))

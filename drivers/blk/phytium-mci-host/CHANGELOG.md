# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.1](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.4.0...phytium-mci-host-v0.4.1) - 2026-08-27

### Other

- *(sdmmc)* unify SDIO protocol and AIC8800 driver ([#2201](https://github.com/rcore-os/tgoskits/pull/2201))

## [0.4.0](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.3.5...phytium-mci-host-v0.4.0) - 2026-08-20

### Added

- *(dma-api)* [**breaking**] add device DMA coherency with uncached-alias remap ([#2106](https://github.com/rcore-os/tgoskits/pull/2106))

## [0.3.5](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.3.4...phytium-mci-host-v0.3.5) - 2026-08-09

### Other

- updated the following local packages: sdmmc-protocol, dma-api, mmio-api, sdio-host2

## [0.3.4](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.3.3...phytium-mci-host-v0.3.4) - 2026-08-03

### Fixed

- *(dma-api)* retire legacy axdma release paths ([#1796](https://github.com/rcore-os/tgoskits/pull/1796))

### Other

- *(block)* adopt IRQ-driven multi-queue runtime ([#1768](https://github.com/rcore-os/tgoskits/pull/1768))

### Changed

- Move block I/O to owned DMA and acknowledged-IRQ progression with a
  controller-lifetime 4 KiB IDMAC descriptor ring.
- Keep the validated 32-bit DMA mask and quarantine ownership when recovery
  cannot prove that DMA is quiescent.
- Build only the active IDMAC chain for each request and route acknowledgement
  exclusively through the owned IRQ endpoint.
- Match all shared protocol progress and bus-width states exhaustively instead
  of treating an unknown terminal state as pending or an unknown width as
  1-bit.

### Removed

- Remove FIFO block fallback, cloned DMA capabilities, and synchronous
  completion polling.
- Remove the direct host `handle_irq` compatibility entry point.

## [0.3.3](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.3.2...phytium-mci-host-v0.3.3) - 2026-07-23

### Other

- updated the following local packages: sdmmc-protocol

## [0.3.2](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.3.1...phytium-mci-host-v0.3.2) - 2026-07-08

### Other

- updated the following local packages: dma-api, sdio-host2, rdif-block, sdmmc-protocol

## [0.3.1](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.3.0...phytium-mci-host-v0.3.1) - 2026-07-07

### Other

- *(sdmmc-protocol)* split SDIO and RDIF capability modules ([#1486](https://github.com/rcore-os/tgoskits/pull/1486))

## [0.3.0](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.2.0...phytium-mci-host-v0.3.0) - 2026-07-02

### Other

- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.1.5...phytium-mci-host-v0.2.0) - 2026-06-27

### Added

- *(sdmmc)* implement native host2 RDIF path

## [0.1.5](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.1.4...phytium-mci-host-v0.1.5) - 2026-06-23

### Other

- updated the following local packages: dma-api

## [0.1.4](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.1.3...phytium-mci-host-v0.1.4) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.1.3](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.1.2...phytium-mci-host-v0.1.3) - 2026-06-09

### Other

- updated the following local packages: dma-api

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.1.1...phytium-mci-host-v0.1.2) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))

### Other

- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(dma-api)* split coherent and streaming DMA APIs ([#932](https://github.com/rcore-os/tgoskits/pull/932))

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.1](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.5.0...sdhci-host-v0.5.1) - 2026-08-27

### Other

- *(sdmmc)* unify SDIO protocol and AIC8800 driver ([#2201](https://github.com/rcore-os/tgoskits/pull/2201))

## [0.5.0](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.4.4...sdhci-host-v0.5.0) - 2026-08-20

### Added

- *(dma-api)* [**breaking**] add device DMA coherency with uncached-alias remap ([#2106](https://github.com/rcore-os/tgoskits/pull/2106))

### Fixed

- *(sdmmc)* align Rockchip reset failure lifecycle ([#1987](https://github.com/rcore-os/tgoskits/pull/1987))

## [0.4.4](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.4.3...sdhci-host-v0.4.4) - 2026-08-09

### Other

- updated the following local packages: sdmmc-protocol, dma-api, mmio-api, sdio-host2, rdif-block

## [0.4.3](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.4.2...sdhci-host-v0.4.3) - 2026-08-03

### Fixed

- *(dma-api)* retire legacy axdma release paths ([#1796](https://github.com/rcore-os/tgoskits/pull/1796))

### Other

- *(block)* adopt IRQ-driven multi-queue runtime ([#1768](https://github.com/rcore-os/tgoskits/pull/1768))

### Changed

- Add an explicit block-transfer policy so RK3588 DWCMSHC can require ADMA2
  and reject every FIFO fallback.
- Remove the raw submit/poll block compatibility API; block I/O now enters
  through the owned-DMA RDIF adapter and advances only from IRQ/deadline events.
- Split host2 transaction ownership from bus-operation state machines, and
  split DMA request lifecycle from FIFO progress and descriptor policy.
- Move controller, DMA, and crate tests out of production modules.
- Make the `SdMmcIrqHost` capability enable and disable the physical SDHCI
  signal masks instead of inheriting the no-op default implementation.
- Use only preallocated 32-bit, 64-bit, or v4 ADMA2 descriptors and enforce
  DMA mask, alignment, descriptor-count, and 128 MiB boundary limits.
- Route acknowledgement exclusively through the owned IRQ endpoint.
- Keep the depth-one ADMA2 table under controller ownership and pass each
  transfer shape directly into command submission instead of moving
  descriptor and transient command state through parallel fields.
- Match all shared protocol progress and bus-width states exhaustively.

### Removed

- Remove PIO block fallback and FIFO DMA compatibility code.
- Remove the direct host `handle_irq` compatibility entry point.

## [0.4.2](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.4.1...sdhci-host-v0.4.2) - 2026-07-23

### Other

- updated the following local packages: sdmmc-protocol

## [0.4.1](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.4.0...sdhci-host-v0.4.1) - 2026-07-08

### Other

- updated the following local packages: dma-api, sdio-host2, rdif-block, sdmmc-protocol

## [0.4.0](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.3.0...sdhci-host-v0.4.0) - 2026-07-07

### Other

- *(drivers)* split Rockchip reset capability ([#1509](https://github.com/rcore-os/tgoskits/pull/1509))
- *(sdmmc-protocol)* split SDIO and RDIF capability modules ([#1486](https://github.com/rcore-os/tgoskits/pull/1486))

## [0.3.0](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.2.0...sdhci-host-v0.3.0) - 2026-07-02

### Other

- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.1.5...sdhci-host-v0.2.0) - 2026-06-27

### Added

- *(sdmmc)* implement native host2 RDIF path

## [0.1.5](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.1.4...sdhci-host-v0.1.5) - 2026-06-23

### Other

- updated the following local packages: dma-api

## [0.1.4](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.1.3...sdhci-host-v0.1.4) - 2026-06-22

### Fixed

- *(sdhci-host)* preserve fifo irq error status ([#1291](https://github.com/rcore-os/tgoskits/pull/1291))

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.1.3](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.1.2...sdhci-host-v0.1.3) - 2026-06-09

### Other

- updated the following local packages: dma-api

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.1.1...sdhci-host-v0.1.2) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))

### Other

- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(dma-api)* split coherent and streaming DMA APIs ([#932](https://github.com/rcore-os/tgoskits/pull/932))

## [0.1.0](https://github.com/rcore-os/tgoskits/releases/tag/sdhci-host-v0.1.0) - 2026-05-16

### Added

- *(sdmmc)* add reusable SD/MMC protocol and host drivers ([#538](https://github.com/rcore-os/tgoskits/pull/538))

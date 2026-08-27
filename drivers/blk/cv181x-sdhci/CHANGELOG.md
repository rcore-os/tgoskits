# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.7](https://github.com/rcore-os/tgoskits/compare/cv181x-sdhci-v0.1.6...cv181x-sdhci-v0.1.7) - 2026-08-27

### Other

- *(sdmmc)* unify SDIO protocol and AIC8800 driver ([#2201](https://github.com/rcore-os/tgoskits/pull/2201))

## [0.1.6](https://github.com/rcore-os/tgoskits/compare/cv181x-sdhci-v0.1.5...cv181x-sdhci-v0.1.6) - 2026-08-20

### Other

- updated the following local packages: mmio-api, dma-api, sdmmc-protocol, sdhci-host

## [0.1.5](https://github.com/rcore-os/tgoskits/compare/cv181x-sdhci-v0.1.4...cv181x-sdhci-v0.1.5) - 2026-08-09

### Other

- updated the following local packages: sdmmc-protocol, dma-api, mmio-api, sdio-host2, sdhci-host

## [0.1.4](https://github.com/rcore-os/tgoskits/compare/cv181x-sdhci-v0.1.3...cv181x-sdhci-v0.1.4) - 2026-08-03

### Other

- *(cv181x-sdhci)* use tock-registers ([#1789](https://github.com/rcore-os/tgoskits/pull/1789))
- *(block)* adopt IRQ-driven multi-queue runtime ([#1768](https://github.com/rcore-os/tgoskits/pull/1768))

### Changed

- Forward owned DMA and IRQ lifecycle operations to the ADMA2-only SDHCI core.
- Split CV181x pad/power/PHY and clock policy into focused modules.

### Removed

- Remove the local polling adapter and cloned DMA block path.

## [0.1.3](https://github.com/rcore-os/tgoskits/compare/cv181x-sdhci-v0.1.2...cv181x-sdhci-v0.1.3) - 2026-07-23

### Other

- updated the following local packages: sdmmc-protocol, sdhci-host

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/cv181x-sdhci-v0.1.1...cv181x-sdhci-v0.1.2) - 2026-07-08

### Other

- updated the following local packages: dma-api, sdio-host2, rdif-block, sdmmc-protocol, sdhci-host

## [0.1.1](https://github.com/rcore-os/tgoskits/compare/cv181x-sdhci-v0.1.0...cv181x-sdhci-v0.1.1) - 2026-07-07

### Other

- *(sdmmc-protocol)* split SDIO and RDIF capability modules ([#1486](https://github.com/rcore-os/tgoskits/pull/1486))

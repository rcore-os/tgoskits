# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.7](https://github.com/rcore-os/tgoskits/compare/starfive-jh7110-dwmmc-v0.1.6...starfive-jh7110-dwmmc-v0.1.7) - 2026-08-27

### Other

- *(sdmmc)* unify SDIO protocol and AIC8800 driver ([#2201](https://github.com/rcore-os/tgoskits/pull/2201))

## [0.1.6](https://github.com/rcore-os/tgoskits/compare/starfive-jh7110-dwmmc-v0.1.5...starfive-jh7110-dwmmc-v0.1.6) - 2026-08-20

### Other

- updated the following local packages: dma-api, sdmmc-protocol, dwmmc-host

## [0.1.5](https://github.com/rcore-os/tgoskits/compare/starfive-jh7110-dwmmc-v0.1.4...starfive-jh7110-dwmmc-v0.1.5) - 2026-08-09

### Other

- updated the following local packages: sdmmc-protocol, dwmmc-host, dma-api, sdio-host2

## [0.1.4](https://github.com/rcore-os/tgoskits/compare/starfive-jh7110-dwmmc-v0.1.3...starfive-jh7110-dwmmc-v0.1.4) - 2026-08-03

### Other

- *(block)* adopt IRQ-driven multi-queue runtime ([#1768](https://github.com/rcore-os/tgoskits/pull/1768))

### Changed

- Keep only JH7110 clock, bus-width, voltage, and profile policy while
  delegating owned IDMAC and IRQ progression to `dwmmc-host`.

### Removed

- Remove the local polling adapter and duplicate block data path.

## [0.1.3](https://github.com/rcore-os/tgoskits/compare/starfive-jh7110-dwmmc-v0.1.2...starfive-jh7110-dwmmc-v0.1.3) - 2026-07-23

### Other

- updated the following local packages: sdmmc-protocol, dwmmc-host

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/starfive-jh7110-dwmmc-v0.1.1...starfive-jh7110-dwmmc-v0.1.2) - 2026-07-08

### Other

- updated the following local packages: dma-api, sdio-host2, rdif-block, sdmmc-protocol, dwmmc-host

## [0.1.1](https://github.com/rcore-os/tgoskits/compare/starfive-jh7110-dwmmc-v0.1.0...starfive-jh7110-dwmmc-v0.1.1) - 2026-07-07

### Other

- updated the following local packages: sdmmc-protocol, dwmmc-host, dma-api, sdio-host2, rdif-block

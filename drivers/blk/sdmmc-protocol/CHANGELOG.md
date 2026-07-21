# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.2](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.4.1...sdmmc-protocol-v0.4.2) - 2026-07-21

### Other

- *(ci)* update Rust nightly to 2026-07-15 ([#1626](https://github.com/rcore-os/tgoskits/pull/1626))

## [0.4.1](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.4.0...sdmmc-protocol-v0.4.1) - 2026-07-08

### Other

- updated the following local packages: dma-api, sdio-host2, rdif-block

## [0.4.0](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.3.0...sdmmc-protocol-v0.4.0) - 2026-07-07

### Added

- *(cv181x-sdhci)* add SG2002 SD driver ([#1482](https://github.com/rcore-os/tgoskits/pull/1482))

### Other

- *(sdmmc-protocol)* split SDIO and RDIF capability modules ([#1486](https://github.com/rcore-os/tgoskits/pull/1486))

## [0.3.0](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.2.0...sdmmc-protocol-v0.3.0) - 2026-07-02

### Fixed

- *(ci)* prevent Starry qemu hangs in IRQ paths ([#1431](https://github.com/rcore-os/tgoskits/pull/1431))

### Other

- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.1.3...sdmmc-protocol-v0.2.0) - 2026-06-27

### Added

- *(sdmmc)* implement native host2 RDIF path

## [0.1.3](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.1.2...sdmmc-protocol-v0.1.3) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.1.1...sdmmc-protocol-v0.1.2) - 2026-06-03

### Other

- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))

## [0.1.0](https://github.com/rcore-os/tgoskits/releases/tag/sdmmc-protocol-v0.1.0) - 2026-05-16

### Added

- *(sdmmc)* add reusable SD/MMC protocol and host drivers ([#538](https://github.com/rcore-os/tgoskits/pull/538))

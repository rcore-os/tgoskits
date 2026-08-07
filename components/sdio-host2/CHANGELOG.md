# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.5](https://github.com/rcore-os/tgoskits/compare/sdio-host2-v0.1.4...sdio-host2-v0.1.5) - 2026-08-03

### Other

- *(block)* adopt IRQ-driven multi-queue runtime ([#1768](https://github.com/rcore-os/tgoskits/pull/1768))

### Changed

- Replace request polling with explicit submission, acknowledged-IRQ, and
  bounded register-retry progress causes.
- Require rejected owned-DMA submissions to return the original transaction;
  remove the legacy ownership-consuming fallback.
- Make `BusWidth` an exhaustive hardware protocol set so every host must
  encode newly added widths explicitly instead of silently guessing.

## [0.1.4](https://github.com/rcore-os/tgoskits/compare/sdio-host2-v0.1.3...sdio-host2-v0.1.4) - 2026-07-08

### Other

- updated the following local packages: dma-api

## [0.1.3](https://github.com/rcore-os/tgoskits/compare/sdio-host2-v0.1.2...sdio-host2-v0.1.3) - 2026-07-07

### Other

- updated the following local packages: dma-api

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/sdio-host2-v0.1.1...sdio-host2-v0.1.2) - 2026-07-02

### Other

- updated the following local packages: dma-api

## [0.1.1](https://github.com/rcore-os/tgoskits/compare/sdio-host2-v0.1.0...sdio-host2-v0.1.1) - 2026-06-27

### Other

- updated the following local packages: dma-api

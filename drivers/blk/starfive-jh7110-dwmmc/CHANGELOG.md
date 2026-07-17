# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Require the protocol's initialized-card capability before the RDIF adapter
  can construct its interrupt-backed runtime device.
- Migrate the RDIF adapter to the 0.12 owned interrupt contract, delegate IDMAC
  queue constraints and typed recovery to DWMMC, and expose timed
  initialization without a synchronous reset wrapper.

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/starfive-jh7110-dwmmc-v0.1.1...starfive-jh7110-dwmmc-v0.1.2) - 2026-07-08

### Other

- updated the following local packages: dma-api, sdio-host2, rdif-block, sdmmc-protocol, dwmmc-host

## [0.1.1](https://github.com/rcore-os/tgoskits/compare/starfive-jh7110-dwmmc-v0.1.0...starfive-jh7110-dwmmc-v0.1.1) - 2026-07-07

### Other

- updated the following local packages: sdmmc-protocol, dwmmc-host, dma-api, sdio-host2, rdif-block

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.4](https://github.com/rcore-os/tgoskits/compare/aic8800-v0.2.3...aic8800-v0.2.4) - 2026-07-08

### Other

- updated the following local packages: ax-kspin, dma-api, rdif-eth, rd-net

## [0.2.3](https://github.com/rcore-os/tgoskits/compare/aic8800-v0.2.2...aic8800-v0.2.3) - 2026-07-07

### Other

- updated the following local packages: ax-kspin, dma-api, rdif-eth, rd-net

## [0.2.2](https://github.com/rcore-os/tgoskits/compare/aic8800-v0.2.1...aic8800-v0.2.2) - 2026-07-02

### Other

- updated the following local packages: ax-kspin, rdif-eth, rd-net, dma-api

## [0.2.1](https://github.com/rcore-os/tgoskits/compare/aic8800-v0.2.0...aic8800-v0.2.1) - 2026-06-27

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/aic8800-v0.1.1...aic8800-v0.2.0) - 2026-06-23

### Added

- *(aic8800)* AIC8800DC SoftAP for SG2002 — boot AP, SSH + HTTP, client reconnect ([#1318](https://github.com/rcore-os/tgoskits/pull/1318))

### Other

- *(ax-net)* add locking and concurrency documentation and remove deprecated interfaces ([#1340](https://github.com/rcore-os/tgoskits/pull/1340))

## [0.1.1](https://github.com/rcore-os/tgoskits/compare/aic8800-v0.1.0...aic8800-v0.1.1) - 2026-06-22

### Added

- runtime Wi-Fi AP/STA mode switch for AIC8800 on SG2002 (LicheeRV Nano) ([#1266](https://github.com/rcore-os/tgoskits/pull/1266))

### Fixed

- *(wifi)* D80 EAPOL TX + SDIO bus recovery, quiet per-frame logging ([#1276](https://github.com/rcore-os/tgoskits/pull/1276))

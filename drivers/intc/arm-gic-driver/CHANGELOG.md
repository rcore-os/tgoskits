# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Correct the ICH and ICC IDbits fields to their architectural three-bit widths
  so they no longer overlap PREbits or SEIS.

## [0.17.9](https://github.com/rcore-os/tgoskits/compare/arm-gic-driver-v0.17.8...arm-gic-driver-v0.17.9) - 2026-07-10

### Added

- *(msi)* add hierarchical MSI-X irq domains ([#1526](https://github.com/rcore-os/tgoskits/pull/1526))

## [0.17.8](https://github.com/rcore-os/tgoskits/compare/arm-gic-driver-v0.17.7...arm-gic-driver-v0.17.8) - 2026-07-07

### Added

- *(msi)* add aarch64 MSI-X registration ([#1522](https://github.com/rcore-os/tgoskits/pull/1522))

## [0.17.7](https://github.com/rcore-os/tgoskits/compare/arm-gic-driver-v0.17.6...arm-gic-driver-v0.17.7) - 2026-07-02

### Fixed

- *(somehal)* validate GIC runtime INTIDs
- *(irq)* close domain runtime review gaps

### Other

- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))
- *(rdif-intc)* accept controller irq domains from callers

## [0.17.6](https://github.com/rcore-os/tgoskits/compare/arm-gic-driver-v0.17.5...arm-gic-driver-v0.17.6) - 2026-06-23

### Fixed

- *(platform)* support AArch64 HVF timer boot ([#1334](https://github.com/rcore-os/tgoskits/pull/1334))

## [0.17.5](https://github.com/rcore-os/tgoskits/compare/arm-gic-driver-v0.17.4...arm-gic-driver-v0.17.5) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.17.4](https://github.com/rcore-os/tgoskits/compare/arm-gic-driver-v0.17.3...arm-gic-driver-v0.17.4) - 2026-06-12

### Other

- updated the following local packages: rdif-intc

## [0.17.3](https://github.com/rcore-os/tgoskits/compare/arm-gic-driver-v0.17.2...arm-gic-driver-v0.17.3) - 2026-06-09

### Other

- updated the following local packages: rdif-intc

## [0.17.2](https://github.com/rcore-os/tgoskits/compare/arm-gic-driver-v0.17.1...arm-gic-driver-v0.17.2) - 2026-06-03

### Other

- *(starry)* route HAL access through ax-runtime ([#963](https://github.com/rcore-os/tgoskits/pull/963))

## [0.17.0](https://github.com/drivercraft/sparreal-os/compare/arm-gic-driver-v0.16.4...arm-gic-driver-v0.17.0) - 2026-03-05

### Other

- Dev/drv ([#32](https://github.com/drivercraft/sparreal-os/pull/32))
- ✨ feat: 重构设备驱动接口，移除 open/close 方法，添加 name 方法 ([#25](https://github.com/drivercraft/sparreal-os/pull/25))

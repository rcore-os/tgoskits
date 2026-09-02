# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Breaking:** replace the kernel-address-space tuple with a typed, fallible
  platform layout that publishes disjoint user and page-table-backed kernel
  ranges.

## [0.13.1](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.13.0...ax-plat-v0.13.1) - 2026-08-27

### Other

- *(runtime)* make IRQ and multitasking mandatory ([#2188](https://github.com/rcore-os/tgoskits/pull/2188))

## [0.13.0](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.12.3...ax-plat-v0.13.0) - 2026-08-20

### Added

- *(dma-api)* [**breaking**] add device DMA coherency with uncached-alias remap ([#2106](https://github.com/rcore-os/tgoskits/pull/2106))

### Other

- *(serial)* establish bounded owner-affine UART runtime ([#2076](https://github.com/rcore-os/tgoskits/pull/2076))
- *(errors)* introduce domain-owned error boundaries ([#2024](https://github.com/rcore-os/tgoskits/pull/2024))
- *(sync)* move lock implementation into ax-task ([#1962](https://github.com/rcore-os/tgoskits/pull/1962))
- *(sync)* unify lock primitives in ax-sync ([#1956](https://github.com/rcore-os/tgoskits/pull/1956))

## [0.12.3](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.12.2...ax-plat-v0.12.3) - 2026-08-09

### Fixed

- *(ax-plat)* add corrected scheduler clock source ([#1900](https://github.com/rcore-os/tgoskits/pull/1900))

### Other

- *(ax-ipi)* establish typed IPI publication transport ([#1916](https://github.com/rcore-os/tgoskits/pull/1916))
- *(axvm)* unify guest devices and AArch64 timer ownership ([#1717](https://github.com/rcore-os/tgoskits/pull/1717))

## [0.12.2](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.12.1...ax-plat-v0.12.2) - 2026-08-03

### Fixed

- *(dma-api)* retire legacy axdma release paths ([#1796](https://github.com/rcore-os/tgoskits/pull/1796))
- *(ax-plat)* pin IRQ context CPU lookup ([#1721](https://github.com/rcore-os/tgoskits/pull/1721))
- *(someboot)* establish runtime CPU topology mapping ([#1710](https://github.com/rcore-os/tgoskits/pull/1710))

## [0.12.1](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.12.0...ax-plat-v0.12.1) - 2026-07-23

### Other

- *(cpu-local)* extract per-CPU register ownership ([#1662](https://github.com/rcore-os/tgoskits/pull/1662))

## [0.12.0](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.11.0...ax-plat-v0.12.0) - 2026-07-10

### Added

- *(msi)* add hierarchical MSI-X irq domains ([#1526](https://github.com/rcore-os/tgoskits/pull/1526))

## [0.11.0](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.10.0...ax-plat-v0.11.0) - 2026-07-08

### Fixed

- *(platforms)* route DMA cache sync through platform cache ops ([#1542](https://github.com/rcore-os/tgoskits/pull/1542))

## [0.10.0](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.9.2...ax-plat-v0.10.0) - 2026-07-08

### Added

- *(loongarch64)* add LS2K1000 physical board support ([#1368](https://github.com/rcore-os/tgoskits/pull/1368))

## [0.9.2](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.9.1...ax-plat-v0.9.2) - 2026-07-08

### Other

- updated the following local packages: ax-kspin, rdrive

## [0.9.1](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.9.0...ax-plat-v0.9.1) - 2026-07-07

### Other

- *(platforms)* move someboot and somehal-macros and add documents ([#1485](https://github.com/rcore-os/tgoskits/pull/1485))
- Dev might sleep enhance ([#1480](https://github.com/rcore-os/tgoskits/pull/1480))

## [0.9.0](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.8.0...ax-plat-v0.9.0) - 2026-07-02

### Added

- *(irq-framework)* use domain-scoped irq ids
- *(axvisor)* support LoongArch Linux guest on QEMU ([#1207](https://github.com/rcore-os/tgoskits/pull/1207))

### Fixed

- *(ax-hal)* route typed IPI ids through platform irq

### Other

- *(irq-framework)* require boxed IRQ callbacks ([#1452](https://github.com/rcore-os/tgoskits/pull/1452))

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.7.0...ax-plat-v0.8.0) - 2026-06-27

### Added

- *(ax-runtime)* generate banner build info ([#1373](https://github.com/rcore-os/tgoskits/pull/1373))

### Other

- *(platform)* remove ax-config from dynamic runtime path ([#1387](https://github.com/rcore-os/tgoskits/pull/1387))
- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.6.4...ax-plat-v0.7.0) - 2026-06-23

### Added

- *(starry)* support reboot syscall ([#1358](https://github.com/rcore-os/tgoskits/pull/1358))

## [0.6.4](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.6.3...ax-plat-v0.6.4) - 2026-06-22

### Other

- updated the following local packages: rdrive

## [0.6.3](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.6.2...ax-plat-v0.6.3) - 2026-06-12

### Added

- *(irq)* enhance IRQ request handling and state restoration logic
- *(axruntime)* add runtime IRQ registration adapters

### Fixed

- *(axtask)* use monotonic deadlines for sleeps ([#1240](https://github.com/rcore-os/tgoskits/pull/1240))

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.6.1...ax-plat-v0.6.2) - 2026-06-11

### Other

- updated the following local packages: ax-plat-macros

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.6.0...ax-plat-v0.6.1) - 2026-06-09

### Other

- updated the following local packages: ax-kernel-guard, ax-percpu, ax-kspin, rdrive

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.5.8...ax-plat-v0.6.0) - 2026-06-03

### Added

- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))

### Other

- Implement platform-specific IRQ handling and architecture setup ([#979](https://github.com/rcore-os/tgoskits/pull/979))

## [0.5.8](https://github.com/rcore-os/tgoskits/compare/ax-plat-v0.5.7...ax-plat-v0.5.8) - 2026-05-15

### Other

- updated the following local packages: ax-kspin, ax-handler-table

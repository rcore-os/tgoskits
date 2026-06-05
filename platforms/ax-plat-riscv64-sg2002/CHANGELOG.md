# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0](https://github.com/rcore-os/tgoskits/compare/ax-plat-riscv64-sg2002-v0.3.6...ax-plat-riscv64-sg2002-v0.4.0) - 2026-06-03

### Added

- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))

### Fixed

- *(repo)* normalize allocator and RISC-V dependencies ([#1021](https://github.com/rcore-os/tgoskits/pull/1021))

### Other

- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- Implement platform-specific IRQ handling and architecture setup ([#979](https://github.com/rcore-os/tgoskits/pull/979))

## [0.3.6](https://github.com/rcore-os/tgoskits/compare/ax-plat-riscv64-sg2002-v0.3.5...ax-plat-riscv64-sg2002-v0.3.6) - 2026-05-22

### Added

- add sg2002 USB UVC camera with ESP-compatible ioctl ([#791](https://github.com/rcore-os/tgoskits/pull/791))

## [0.3.5](https://github.com/rcore-os/tgoskits/compare/ax-plat-riscv64-sg2002-v0.3.4...ax-plat-riscv64-sg2002-v0.3.5) - 2026-05-19

### Other

- updated the following local packages: ax-riscv-plic, ax-cpu

## [0.3.4](https://github.com/rcore-os/tgoskits/compare/ax-plat-riscv64-sg2002-v0.3.3...ax-plat-riscv64-sg2002-v0.3.4) - 2026-05-15

### Other

- updated the following local packages: ax-kspin, ax-riscv-plic, ax-config-macros, ax-cpu, ax-plat

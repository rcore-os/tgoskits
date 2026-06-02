# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.10](https://github.com/rcore-os/tgoskits/compare/ax-plat-riscv64-qemu-virt-v0.5.9...ax-plat-riscv64-qemu-virt-v0.5.10) - 2026-06-02

### Added

- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))

### Fixed

- *(repo)* normalize allocator and RISC-V dependencies ([#1021](https://github.com/rcore-os/tgoskits/pull/1021))

### Other

- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- Implement platform-specific IRQ handling and architecture setup ([#979](https://github.com/rcore-os/tgoskits/pull/979))

## [0.5.9](https://github.com/rcore-os/tgoskits/compare/ax-plat-riscv64-qemu-virt-v0.5.8...ax-plat-riscv64-qemu-virt-v0.5.9) - 2026-05-22

### Other

- Remove RISC-V QEMU Virt platform files and update references ([#833](https://github.com/rcore-os/tgoskits/pull/833))

## [0.5.8](https://github.com/rcore-os/tgoskits/compare/ax-plat-riscv64-qemu-virt-v0.5.7...ax-plat-riscv64-qemu-virt-v0.5.8) - 2026-05-19

### Other

- updated the following local packages: ax-riscv-plic, ax-cpu

## [0.5.7](https://github.com/rcore-os/tgoskits/compare/ax-plat-riscv64-qemu-virt-v0.5.6...ax-plat-riscv64-qemu-virt-v0.5.7) - 2026-05-15

### Added

- *(drivers)* migrate Sparreal driver crates ([#540](https://github.com/rcore-os/tgoskits/pull/540))
- *(irq)* pass IRQ/event number to registered handlers

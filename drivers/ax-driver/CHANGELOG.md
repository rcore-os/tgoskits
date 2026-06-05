# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.6.1...ax-driver-v0.6.2) - 2026-06-05

### Other

- *(ax-driver)* remove redundant mmio cfg gate ([#1100](https://github.com/rcore-os/tgoskits/pull/1100))

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.6.0...ax-driver-v0.6.1) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))
- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(starryos)* add QEMU K230 boot support ([#1046](https://github.com/rcore-os/tgoskits/pull/1046))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))
- *(some-serial)* add Rockchip FIQ debugger UART ([#980](https://github.com/rcore-os/tgoskits/pull/980))

### Other

- *(platform)* migrate riscv64 qemu to dynamic platform ([#1085](https://github.com/rcore-os/tgoskits/pull/1085))
- *(platform)* remove static aarch64 platforms ([#1074](https://github.com/rcore-os/tgoskits/pull/1074))
- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.5.14...ax-driver-v0.6.0) - 2026-05-19

### Fixed

- *(starry)* weston bringup fixes + IRQ wakers + AF_UNIX cmsg byte marks ([#509](https://github.com/rcore-os/tgoskits/pull/509))

### Other

- Refactor Clippy integration and enhance package handling ([#738](https://github.com/rcore-os/tgoskits/pull/738))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.5.13...ax-driver-v0.5.14) - 2026-05-15

### Other

- updated the following local packages: ax-driver-input, ax-driver-virtio, ax-alloc, ax-config, axplat-dyn, ax-hal, ax-driver-net, ax-dma

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-driver-v0.5.11...ax-driver-v0.5.12) - 2026-04-27

### Other

- updated the following local packages: ax-alloc

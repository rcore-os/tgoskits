# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-plat-loongarch64-qemu-virt-v0.5.15...ax-plat-loongarch64-qemu-virt-v0.5.16) - 2026-06-23

### Added

- *(starry)* support reboot syscall ([#1358](https://github.com/rcore-os/tgoskits/pull/1358))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-plat-loongarch64-qemu-virt-v0.5.14...ax-plat-loongarch64-qemu-virt-v0.5.15) - 2026-06-22

### Added

- *(starry)* add Wayland app case ([#1160](https://github.com/rcore-os/tgoskits/pull/1160))
- *(ax-plat-loongarch64-qemu-virt)* detect RAM size from the FDT ([#1214](https://github.com/rcore-os/tgoskits/pull/1214))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-plat-loongarch64-qemu-virt-v0.5.13...ax-plat-loongarch64-qemu-virt-v0.5.14) - 2026-06-12

### Fixed

- *(ci)* stabilize x86 Starry QEMU timing ([#1245](https://github.com/rcore-os/tgoskits/pull/1245))
- *(loongarch64)* ack timer irq before dispatch ([#1222](https://github.com/rcore-os/tgoskits/pull/1222))

### Other

- *(ax-driver)* register devices with binding info

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/ax-plat-loongarch64-qemu-virt-v0.5.12...ax-plat-loongarch64-qemu-virt-v0.5.13) - 2026-06-11

### Fixed

- *(starry)* support eBPF ringbuf mmap on LoongArch DMW ([#1208](https://github.com/rcore-os/tgoskits/pull/1208))

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-plat-loongarch64-qemu-virt-v0.5.11...ax-plat-loongarch64-qemu-virt-v0.5.12) - 2026-06-09

### Fixed

- *(axcpu)* preserve loongarch64 LASX state for Git HTTPS ([#1178](https://github.com/rcore-os/tgoskits/pull/1178))

## [0.5.11](https://github.com/rcore-os/tgoskits/compare/ax-plat-loongarch64-qemu-virt-v0.5.10...ax-plat-loongarch64-qemu-virt-v0.5.11) - 2026-06-03

### Added

- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))

### Other

- *(linker)* layer platform runtime and final scripts ([#1075](https://github.com/rcore-os/tgoskits/pull/1075))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- Implement platform-specific IRQ handling and architecture setup ([#979](https://github.com/rcore-os/tgoskits/pull/979))

## [0.5.10](https://github.com/rcore-os/tgoskits/compare/ax-plat-loongarch64-qemu-virt-v0.5.9...ax-plat-loongarch64-qemu-virt-v0.5.10) - 2026-05-22

### Other

- updated the following local packages: ax-cpu

## [0.5.9](https://github.com/rcore-os/tgoskits/compare/ax-plat-loongarch64-qemu-virt-v0.5.8...ax-plat-loongarch64-qemu-virt-v0.5.9) - 2026-05-19

### Other

- updated the following local packages: ax-cpu

## [0.5.8](https://github.com/rcore-os/tgoskits/compare/ax-plat-loongarch64-qemu-virt-v0.5.7...ax-plat-loongarch64-qemu-virt-v0.5.8) - 2026-05-15

### Added

- add support for loongarch64·

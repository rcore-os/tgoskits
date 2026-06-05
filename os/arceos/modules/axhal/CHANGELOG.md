# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-hal-v0.5.15...ax-hal-v0.5.16) - 2026-06-03

### Added

- *(starryos)* expose K230 KPU device ([#1054](https://github.com/rcore-os/tgoskits/pull/1054))
- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))

### Fixed

- *(axbuild)* skip disabled grouped C subcases ([#942](https://github.com/rcore-os/tgoskits/pull/942))

### Other

- *(platform)* migrate riscv64 qemu to dynamic platform ([#1085](https://github.com/rcore-os/tgoskits/pull/1085))
- *(platform)* remove static aarch64 platforms ([#1074](https://github.com/rcore-os/tgoskits/pull/1074))
- *(linker)* layer platform runtime and final scripts ([#1075](https://github.com/rcore-os/tgoskits/pull/1075))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))
- Implement platform-specific IRQ handling and architecture setup ([#979](https://github.com/rcore-os/tgoskits/pull/979))
- *(starry)* route HAL access through ax-runtime ([#963](https://github.com/rcore-os/tgoskits/pull/963))
- *(driver)* move static probes to platform-owned registration ([#937](https://github.com/rcore-os/tgoskits/pull/937))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))
- Refactor workspace structure and update dependencies ([#864](https://github.com/rcore-os/tgoskits/pull/864))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-hal-v0.5.14...ax-hal-v0.5.15) - 2026-05-22

### Added

- *(axplat-aarch64)* GICv3 + CNTV backend for Apple HVF native execution ([#511](https://github.com/rcore-os/tgoskits/pull/511))

### Fixed

- *(axvisor)* recover riscv guest memory faults ([#788](https://github.com/rcore-os/tgoskits/pull/788))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-hal-v0.5.13...ax-hal-v0.5.14) - 2026-05-19

### Other

- updated the following local packages: ax-alloc, ax-page-table-multiarch, ax-cpu, ax-plat-aarch64-qemu-virt, ax-plat-loongarch64-qemu-virt, ax-plat-riscv64-qemu-virt, ax-plat-x86-pc, axplat-dyn

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/ax-hal-v0.5.12...ax-hal-v0.5.13) - 2026-05-15

### Added

- add support for loongarch64·
- *(ax-task)* add stack canary checks for multitask stacks ([#416](https://github.com/rcore-os/tgoskits/pull/416))
- *(runtime)* extend IRQ, RTC, and tty event support ([#287](https://github.com/rcore-os/tgoskits/pull/287))
- *(console)* add interrupt-driven console input ([#343](https://github.com/rcore-os/tgoskits/pull/343))

### Fixed

- remove unnecessary copy of link script
- fix null pointer on qemu aarch64 when booting with ELF
- fix linker script to correct physic addr of segments
- *(console)* keep UART writes raw ([#402](https://github.com/rcore-os/tgoskits/pull/402))

### Other

- remove unused field
- *(arceos-modules)* inherit workspace metadata

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-hal-v0.5.11...ax-hal-v0.5.12) - 2026-04-27

### Added

- *(axvisor)* add loongarch64 qemu support and CI ([#242](https://github.com/rcore-os/tgoskits/pull/242))

### Other

- Unifies breakpoint and debug trap handling across archs ([#244](https://github.com/rcore-os/tgoskits/pull/244))

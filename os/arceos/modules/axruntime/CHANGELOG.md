# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.6.0...ax-runtime-v0.6.1) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))
- *(backtrace)* add showcase workflow ([#1094](https://github.com/rcore-os/tgoskits/pull/1094))

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.5.16...ax-runtime-v0.6.0) - 2026-06-03

### Added

- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))
- *(axtask)* add task stack guard page support ([#811](https://github.com/rcore-os/tgoskits/pull/811))

### Fixed

- *(axvisor)* enable buddy-slab allocator ([#974](https://github.com/rcore-os/tgoskits/pull/974))
- *(axruntime)* initialize the page allocator from the largest free RAM region ([#922](https://github.com/rcore-os/tgoskits/pull/922))
- *(axruntime)* park secondary harts beyond MAX_CPU_NUM instead of panicking ([#919](https://github.com/rcore-os/tgoskits/pull/919))

### Other

- *(platform)* migrate riscv64 qemu to dynamic platform ([#1085](https://github.com/rcore-os/tgoskits/pull/1085))
- *(linker)* layer platform runtime and final scripts ([#1075](https://github.com/rcore-os/tgoskits/pull/1075))
- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- *(axruntime)* remove alloc feature, make it unconditional ([#985](https://github.com/rcore-os/tgoskits/pull/985))
- *(starry)* route HAL access through ax-runtime ([#963](https://github.com/rcore-os/tgoskits/pull/963))
- *(driver)* move static probes to platform-owned registration ([#937](https://github.com/rcore-os/tgoskits/pull/937))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))
- *(axbuild)* use target JSON specs for kernel builds ([#839](https://github.com/rcore-os/tgoskits/pull/839))
- Refactor workspace structure and update dependencies ([#864](https://github.com/rcore-os/tgoskits/pull/864))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.5.15...ax-runtime-v0.5.16) - 2026-05-22

### Other

- *(axbacktrace)* use Backtrace::kind() instead of BacktraceReport ([#748](https://github.com/rcore-os/tgoskits/pull/748))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.5.14...ax-runtime-v0.5.15) - 2026-05-19

### Other

- updated the following local packages: ax-alloc, ax-driver, ax-task, ax-net, axklib, ax-hal, ax-mm, ax-display, ax-fs, ax-fs-ng, ax-input, ax-ipi, ax-net

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.5.13...ax-runtime-v0.5.14) - 2026-05-15

### Other

- updated the following local packages: axbacktrace, ax-alloc, ax-config, ax-hal, ax-mm, ax-fs, ax-fs-ng, ax-log, ax-net, ax-net, ax-plat, ax-driver, ax-task, ax-display, ax-input, ax-ipi

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-runtime-v0.5.11...ax-runtime-v0.5.12) - 2026-04-27

### Added

- *(axvisor)* add loongarch64 qemu support and CI ([#242](https://github.com/rcore-os/tgoskits/pull/242))

### Other

- *(ax-alloc)* fix percpu slab spelling

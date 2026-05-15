# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

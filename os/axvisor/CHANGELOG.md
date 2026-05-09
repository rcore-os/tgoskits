# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.5](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.4...axvisor-v0.5.5) - 2026-04-27

### Added

- *(axvisor)* add loongarch64 qemu support and CI ([#242](https://github.com/rcore-os/tgoskits/pull/242))

### Fixed

- *(axvisor)* wake sleeping vcpus during shutdown ([#206](https://github.com/rcore-os/tgoskits/pull/206))
- *(axvisor)* auto-repair board guest rootfs fsck ([#304](https://github.com/rcore-os/tgoskits/pull/304))
- *(axvisor)* update riscv64-qemu-virt-hv references in source ([#275](https://github.com/rcore-os/tgoskits/pull/275))

### Other

- *(axvisor)* add Linux guest support to the AxVisor riscv64 QEMU test ([#351](https://github.com/rcore-os/tgoskits/pull/351))
- Add RISC-V 64 QEMU Virt platform support ([#293](https://github.com/rcore-os/tgoskits/pull/293))
- Enhance QEMU rootfs handling and architecture feature updates ([#286](https://github.com/rcore-os/tgoskits/pull/286))
- Enhance QEMU rootfs handling and update architecture configurations ([#281](https://github.com/rcore-os/tgoskits/pull/281))

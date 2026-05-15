# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.6](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.5...axvisor-v0.5.6) - 2026-05-15

### Added

- *(axvisor)* support x86_64(VMX) QEMU guest boot ([#526](https://github.com/rcore-os/tgoskits/pull/526))
- *(drivers)* migrate Sparreal driver crates ([#540](https://github.com/rcore-os/tgoskits/pull/540))
- *(axvisor)* Add x86_64 AMD SVM support ([#445](https://github.com/rcore-os/tgoskits/pull/445))
- *(rockchip-soc)* migrate RK3588 clocks ([#384](https://github.com/rcore-os/tgoskits/pull/384))
- support freertos and zephyr on tac-e400 ([#365](https://github.com/rcore-os/tgoskits/pull/365))

### Fixed

- update kernel entry points for FreeRTOS and Zephyr VM configuratons ([#390](https://github.com/rcore-os/tgoskits/pull/390))

### Other

- Refactor architecture and enhance commands for incremental checks ([#532](https://github.com/rcore-os/tgoskits/pull/532))
- Refactor QEMU build configuration and test execution flow ([#527](https://github.com/rcore-os/tgoskits/pull/527))
- fmt vm conifg file and  organize annotations ([#393](https://github.com/rcore-os/tgoskits/pull/393))
- Implement QEMU test orchestration and refactor Axvisor/Starry tests ([#394](https://github.com/rcore-os/tgoskits/pull/394))
- *(axvisor)* inherit workspace dependencies
- Update rootfs archive format and boot arguments for configurations ([#380](https://github.com/rcore-os/tgoskits/pull/380))
- *(starry)* drop outdated and unmaintained stuffs ([#353](https://github.com/rcore-os/tgoskits/pull/353))

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

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.10](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.9...axvisor-v0.5.10) - 2026-06-09

### Added

- *(axvisor)* support dynamic x86_64 QEMU guest boot ([#1166](https://github.com/rcore-os/tgoskits/pull/1166))
- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))

### Other

- *(arceos)* reorganize apps ([#1180](https://github.com/rcore-os/tgoskits/pull/1180))

## [0.5.9](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.8...axvisor-v0.5.9) - 2026-06-03

### Added

- *(irq)* introduce shared IRQ framework ([#1065](https://github.com/rcore-os/tgoskits/pull/1065))
- Enhance SVM support and improve PIT handling for Linux guests ([#1005](https://github.com/rcore-os/tgoskits/pull/1005))
- *(axvisor)* support x86_64 Linux guest boot (vmx) ([#930](https://github.com/rcore-os/tgoskits/pull/930))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))
- *(axvisor)* add PhytiumPi and ROC-RK3568 board tests ([#934](https://github.com/rcore-os/tgoskits/pull/934))
- *(axvisor)* bring up minimal LoongArch ArceOS guest ([#768](https://github.com/rcore-os/tgoskits/pull/768))

### Fixed

- *(axvisor)* enable buddy-slab allocator ([#974](https://github.com/rcore-os/tgoskits/pull/974))
- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))

### Other

- *(platform)* migrate riscv64 qemu to dynamic platform ([#1085](https://github.com/rcore-os/tgoskits/pull/1085))
- *(linker)* layer platform runtime and final scripts ([#1075](https://github.com/rcore-os/tgoskits/pull/1075))
- *(axvisor)* reorganize VM configs into platform-first directory structure ([#1063](https://github.com/rcore-os/tgoskits/pull/1063))
- [AxVisor] add x86_64 UEFI guest support ([#760](https://github.com/rcore-os/tgoskits/pull/760))
- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))
- Implement platform-specific IRQ handling and architecture setup ([#979](https://github.com/rcore-os/tgoskits/pull/979))
- Refactor FDT handling, error management, and improve code clarity ([#966](https://github.com/rcore-os/tgoskits/pull/966))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))
- Refactor workspace structure and update dependencies ([#864](https://github.com/rcore-os/tgoskits/pull/864))

## [0.5.8](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.7...axvisor-v0.5.8) - 2026-05-22

### Added

- *(drivers)* add SD/MMC platform driver support ([#826](https://github.com/rcore-os/tgoskits/pull/826))

### Fixed

- *(repo)* improve rsext4 recovery mount and Axvisor board CI ([#830](https://github.com/rcore-os/tgoskits/pull/830))

### Other

- Revert " fix(repo): improve rsext4 recovery mount and Axvisor board CI ([#830](https://github.com/rcore-os/tgoskits/pull/830))" ([#838](https://github.com/rcore-os/tgoskits/pull/838))
- Remove RISC-V QEMU Virt platform files and update references ([#833](https://github.com/rcore-os/tgoskits/pull/833))

## [0.5.7](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.6...axvisor-v0.5.7) - 2026-05-19

### Added

- *(git)* enhance global Clippy input handling and fallback logic ([#758](https://github.com/rcore-os/tgoskits/pull/758))

### Other

- Refactor Clippy integration and enhance package handling ([#738](https://github.com/rcore-os/tgoskits/pull/738))

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
- fmt vm config file and  organize annotations ([#393](https://github.com/rcore-os/tgoskits/pull/393))
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

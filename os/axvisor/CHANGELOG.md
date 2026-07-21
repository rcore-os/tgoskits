# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.22](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.21...axvisor-v0.5.22) - 2026-07-21

### Added

- *(axvisor)* Enhance AxLoader and Asus NUC15CRH support with fixes ([#1555](https://github.com/rcore-os/tgoskits/pull/1555))

### Fixed

- *(doc)* correct broken Quick Start hyperlink in axvisor READMEs ([#1605](https://github.com/rcore-os/tgoskits/pull/1605))

### Other

- *(x86_vcpu)* select VMX/SVM backend at runtime from CPUID, rem… ([#1629](https://github.com/rcore-os/tgoskits/pull/1629))
- *(axbuild)* 将构建与启动能力收敛到显式配置 ([#1620](https://github.com/rcore-os/tgoskits/pull/1620))
- *(axvmconfig)* introduce configuration errors ([#1597](https://github.com/rcore-os/tgoskits/pull/1597))
- *(axvm)* introduce typed domain errors ([#1590](https://github.com/rcore-os/tgoskits/pull/1590))
- *(axvm)* consolidate architecture-specific code ([#1562](https://github.com/rcore-os/tgoskits/pull/1562))

## [0.5.21](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.20...axvisor-v0.5.21) - 2026-07-10

### Other

- updated the following local packages: ax-driver, axplat-dyn, axplat-dyn, ax-hal, axvm, axbuild, ax-std

## [0.5.20](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.19...axvisor-v0.5.20) - 2026-07-08

### Other

- updated the following local packages: axplat-dyn, axplat-dyn, ax-hal, axbuild, ax-driver, ax-std, axvm

## [0.5.19](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.18...axvisor-v0.5.19) - 2026-07-08

### Other

- updated the following local packages: axbuild, ax-driver, axplat-dyn, axplat-dyn, ax-hal, ax-std, axvm

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.17...axvisor-v0.5.18) - 2026-07-08

### Other

- updated the following local packages: ax-driver, axplat-dyn, axplat-dyn, ax-hal, ax-std, axvm

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.16...axvisor-v0.5.17) - 2026-07-07

### Fixed

- *(ci)* restore Starry ptrace and Axvisor RISC-V tests ([#1521](https://github.com/rcore-os/tgoskits/pull/1521))

### Other

- Remove `ax-feat` crate and redistribute features across runtime, API, and user library layers ([#1513](https://github.com/rcore-os/tgoskits/pull/1513))
- *(platforms)* move someboot and somehal-macros and add documents ([#1485](https://github.com/rcore-os/tgoskits/pull/1485))
- *(axvm)* use generic nested page tables ([#1477](https://github.com/rcore-os/tgoskits/pull/1477))
- remove static platform and axconfig generation, make dynamic platform the only path ([#1478](https://github.com/rcore-os/tgoskits/pull/1478))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.15...axvisor-v0.5.16) - 2026-07-02

### Added

- *(axtest)* simplify kernel test targets ([#1470](https://github.com/rcore-os/tgoskits/pull/1470))
- *(axvisor)* support LoongArch Linux guest on QEMU ([#1207](https://github.com/rcore-os/tgoskits/pull/1207))

### Fixed

- *(axvisor)* gate x86 host fs passthrough prepare

### Other

- *(axvm)* decouple axvisor arch logic ([#1471](https://github.com/rcore-os/tgoskits/pull/1471))
- *(axvm)* move VM boot and memory preparation into axvm ([#1462](https://github.com/rcore-os/tgoskits/pull/1462))
- *(axvm)* redesign guest address layout planning ([#1454](https://github.com/rcore-os/tgoskits/pull/1454))
- *(axvm)* redesign VM lifecycle state machine ([#1447](https://github.com/rcore-os/tgoskits/pull/1447))
- *(platforms)* remove LoongArch static platform ([#1428](https://github.com/rcore-os/tgoskits/pull/1428))
- *(build)* generate build.rs Rust sources with quote ([#1422](https://github.com/rcore-os/tgoskits/pull/1422))
- *(axvm)* route host IRQs with domain metadata

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.14...axvisor-v0.5.15) - 2026-06-27

### Other

- *(axdevice)* unify Device model with indexed dispatch and conflict detect ([#1335](https://github.com/rcore-os/tgoskits/pull/1335))
- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.13...axvisor-v0.5.14) - 2026-06-23

### Other

- Enhance archive extraction logic and add legacy file tests ([#1355](https://github.com/rcore-os/tgoskits/pull/1355))

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.12...axvisor-v0.5.13) - 2026-06-22

### Added

- *(axruntime)* add compiler-backed stack protector support ([#1239](https://github.com/rcore-os/tgoskits/pull/1239))

### Fixed

- *(axvisor)* map qemu high mmio pci windows ([#1289](https://github.com/rcore-os/tgoskits/pull/1289))

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.11...axvisor-v0.5.12) - 2026-06-12

### Other

- *(ax-net)* unify network stack into single net/ax-net crate, r… ([#1203](https://github.com/rcore-os/tgoskits/pull/1203))

## [0.5.11](https://github.com/rcore-os/tgoskits/compare/axvisor-v0.5.10...axvisor-v0.5.11) - 2026-06-11

### Added

- *(axplat-dyn)* add LoongArch64 UEFI dynamic platform ([#1190](https://github.com/rcore-os/tgoskits/pull/1190))

### Fixed

- *(axvisor)* keep LoongArch QEMU on static platform
- fix typos in code and comments across the codebase ([#1206](https://github.com/rcore-os/tgoskits/pull/1206))
- *(axvisor)* avoid svm guest timer calibration stall ([#1205](https://github.com/rcore-os/tgoskits/pull/1205))

### Other

- Revert "feat(axplat-dyn): add LoongArch64 UEFI dynamic platform ([#1190](https://github.com/rcore-os/tgoskits/pull/1190))" ([#1202](https://github.com/rcore-os/tgoskits/pull/1202))
- *(axvisor)* remove obsolete x86 q35 static platform ([#1186](https://github.com/rcore-os/tgoskits/pull/1186))

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

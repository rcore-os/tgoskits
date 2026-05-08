# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.5](https://github.com/rcore-os/tgoskits/compare/axbuild-v0.4.4...axbuild-v0.4.5) - 2026-04-27

### Added

- *(axbuild)* extend sync-lint mixed-ordering checks ([#322](https://github.com/rcore-os/tgoskits/pull/322))
- *(tests)* add busybox test case with sh/ script injection ([#299](https://github.com/rcore-os/tgoskits/pull/299))
- add axconfig.toml for x86-qemu-q35 platform configuration
- *(tests)* enhance QEMU case handling with target-specific build configurations ([#291](https://github.com/rcore-os/tgoskits/pull/291))
- *(axvisor)* add loongarch64 qemu support and CI ([#242](https://github.com/rcore-os/tgoskits/pull/242))
- *(axbuild)* add first-phase sync-lint checks for atomic ordering ([#274](https://github.com/rcore-os/tgoskits/pull/274))
- *(axbuild)* refactor Starry QEMU test-suit flow ([#234](https://github.com/rcore-os/tgoskits/pull/234))

### Fixed

- *(axbuild)* sync qemu rootfs tests with interface changes ([#283](https://github.com/rcore-os/tgoskits/pull/283))
- *(rootfs)* conditionally use unix-specific permissions handling
- report real cpu affinity in proc status ([#267](https://github.com/rcore-os/tgoskits/pull/267))
- *(ci)* restore clippy checks after dev rebase
- update registry dependency resolution to use workspace root and arceos directory

### Other

- *(axvisor)* add Linux guest support to the AxVisor riscv64 QEMU test ([#351](https://github.com/rcore-os/tgoskits/pull/351))
- *(axbuild)* extract shared rootfs helpers into common modules ([#340](https://github.com/rcore-os/tgoskits/pull/340))
- Refactor rootfs handling and update QEMU configurations for Alpine ([#297](https://github.com/rcore-os/tgoskits/pull/297))
- Add RISC-V 64 QEMU Virt platform support ([#293](https://github.com/rcore-os/tgoskits/pull/293))
- Enhance QEMU rootfs handling and architecture feature updates ([#286](https://github.com/rcore-os/tgoskits/pull/286))
- Enhance QEMU rootfs handling and update architecture configurations ([#281](https://github.com/rcore-os/tgoskits/pull/281))
- *(smoltcp)* restore dev version
- *(axbuild)* gate non-host dependencies
- Merge branch 'dev' into debug
- update package versions in Cargo.toml and Cargo.lock to 0.5.9; enhance board directory check in starry_dir function

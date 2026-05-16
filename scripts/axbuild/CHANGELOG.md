# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.6](https://github.com/rcore-os/tgoskits/compare/axbuild-v0.4.5...axbuild-v0.4.6) - 2026-05-15

### Added

- *(axvisor)* support x86_64(VMX) QEMU guest boot ([#526](https://github.com/rcore-os/tgoskits/pull/526))
- Rust cross-compilation pipeline for StarryOS QEMU test cases ([#471](https://github.com/rcore-os/tgoskits/pull/471))
- *(starryos/procfs)* implement /proc/stat, /proc/cpuinfo, /proc/uptime; fix /proc/meminfo and sysinfo() ([#452](https://github.com/rcore-os/tgoskits/pull/452))
- *(axbuild)* support Starry board examples
- *(realtek-rtl8125)* complete OrangePi board bringup ([#404](https://github.com/rcore-os/tgoskits/pull/404))
- *(ax-net-ng)* add ICMP raw socket support ([#368](https://github.com/rcore-os/tgoskits/pull/368))
- *(lockdep)* extend lockdep with task-held tracking and qemu regression coverage ([#415](https://github.com/rcore-os/tgoskits/pull/415))
- *(runtime)* extend IRQ, RTC, and tty event support ([#287](https://github.com/rcore-os/tgoskits/pull/287))
- *(rockchip-soc)* migrate RK3588 clocks ([#384](https://github.com/rcore-os/tgoskits/pull/384))
- add python test pipeline and python-hello test case ([#355](https://github.com/rcore-os/tgoskits/pull/355))
- *(axbuild)* support grouped Starry qemu tests ([#369](https://github.com/rcore-os/tgoskits/pull/369))

### Fixed

- *(axbuild)* prepare all managed QEMU drive rootfs images
- *(axbuild)* fix ld resolving wrong libraries while preparing stagin-rootfs ([#413](https://github.com/rcore-os/tgoskits/pull/413))

### Other

- Merge pull request #554 from rcore-os/feat/sg2002-pr383
- Update C build support and streamline QEMU handling for ArceOS ([#576](https://github.com/rcore-os/tgoskits/pull/576))
- *(axbuild)* centralize build-info loading ([#570](https://github.com/rcore-os/tgoskits/pull/570))
- Refactor platform configuration handling and remove cargo-axplat ([#552](https://github.com/rcore-os/tgoskits/pull/552))
- Enhance build system and add support for RISC-V VisionFive2 platform ([#541](https://github.com/rcore-os/tgoskits/pull/541))
- Enhance Git and CI commands with safe.directory support ([#537](https://github.com/rcore-os/tgoskits/pull/537))
- Refactor architecture and enhance commands for incremental checks ([#532](https://github.com/rcore-os/tgoskits/pull/532))
- Refactor QEMU build configuration and test execution flow ([#527](https://github.com/rcore-os/tgoskits/pull/527))
- Refactor Axvisor and Starry rootfs handling and QEMU configurations ([#433](https://github.com/rcore-os/tgoskits/pull/433))
- Refactor build configurations and enhance network and syscall features ([#423](https://github.com/rcore-os/tgoskits/pull/423))
- Implement vfork, getpgrp, and time syscalls with test enhancements ([#409](https://github.com/rcore-os/tgoskits/pull/409))
- Refactor prebuild scripts, enhance test configurations, and improve QEMU discovery ([#414](https://github.com/rcore-os/tgoskits/pull/414))
- Refactor rootfs handling and modularize support functions ([#399](https://github.com/rcore-os/tgoskits/pull/399))
- Implement QEMU test orchestration and refactor Axvisor/Starry tests ([#394](https://github.com/rcore-os/tgoskits/pull/394))
- Merge branch 'rcore-os:dev' into dev
- Merge pull request #366 from rcore-os/fix-deps
- Update rootfs archive format and boot arguments for configurations ([#380](https://github.com/rcore-os/tgoskits/pull/380))
- *(starry)* drop outdated and unmaintained stuffs ([#353](https://github.com/rcore-os/tgoskits/pull/353))

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

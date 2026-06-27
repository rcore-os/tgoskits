# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.15](https://github.com/rcore-os/tgoskits/compare/axbuild-v0.4.14...axbuild-v0.4.15) - 2026-06-27

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))
- *(claw-code)* respect managed rootfs image paths ([#1389](https://github.com/rcore-os/tgoskits/pull/1389))
- *(starryos)* bound signal interrupt qemu test wait ([#1374](https://github.com/rcore-os/tgoskits/pull/1374))

### Other

- *(starry)* cover UV 0.11 and subcommand behavior in functional tests. ([#1211](https://github.com/rcore-os/tgoskits/pull/1211))
- *(platforms)* remove VisionFive2 static platform ([#1371](https://github.com/rcore-os/tgoskits/pull/1371))
- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.4.14](https://github.com/rcore-os/tgoskits/compare/axbuild-v0.4.13...axbuild-v0.4.14) - 2026-06-23

### Added

- *(axbuild, starry)* add rootfs resize and macOS self-build workflow ([#1333](https://github.com/rcore-os/tgoskits/pull/1333))
- *(axbuild)* use ITS companion files for Starry uImage ([#1349](https://github.com/rcore-os/tgoskits/pull/1349))

### Other

- Enhance archive extraction logic and add legacy file tests ([#1355](https://github.com/rcore-os/tgoskits/pull/1355))
- *(axbuild)* modularize command implementation ([#1347](https://github.com/rcore-os/tgoskits/pull/1347))

## [0.4.13](https://github.com/rcore-os/tgoskits/compare/axbuild-v0.4.12...axbuild-v0.4.13) - 2026-06-22

### Added

- *(starry)* add Wayland app case ([#1160](https://github.com/rcore-os/tgoskits/pull/1160))
- *(axbuild)* add standalone axloader command ([#1312](https://github.com/rcore-os/tgoskits/pull/1312))
- *(axbuild)* internalize Starry kallsyms flow ([#1309](https://github.com/rcore-os/tgoskits/pull/1309))
- Enhance HTTP bootloader with inspection, publishing, and features ([#1148](https://github.com/rcore-os/tgoskits/pull/1148))
- *(ax-net)* add multi-interface support with per-interface routing, DNS, and SO_BINDTODEVICE ([#1244](https://github.com/rcore-os/tgoskits/pull/1244))
- *(axruntime)* add compiler-backed stack protector support ([#1239](https://github.com/rcore-os/tgoskits/pull/1239))
- AIC8800 Wi-Fi SoftAP for SG2002 (LicheeRV Nano) ([#1185](https://github.com/rcore-os/tgoskits/pull/1185))

### Fixed

- *(starry)* route app qemu through dynamic boot ([#1267](https://github.com/rcore-os/tgoskits/pull/1267))
- *(starry)* align app qemu boot flow and own BPF JIT memory ([#1256](https://github.com/rcore-os/tgoskits/pull/1256))

### Other

- Feat/x86 64 ptrace clean ([#1062](https://github.com/rcore-os/tgoskits/pull/1062))
- *(arceos)* clean up Hermit remnants ([#1300](https://github.com/rcore-os/tgoskits/pull/1300))
- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.4.12](https://github.com/rcore-os/tgoskits/compare/axbuild-v0.4.11...axbuild-v0.4.12) - 2026-06-12

### Added

- *(starry)* add axbuild kmod support ([#1232](https://github.com/rcore-os/tgoskits/pull/1232))
- *(axbuild)* extend sync-lint relaxed synchronization checks ([#1236](https://github.com/rcore-os/tgoskits/pull/1236))
- *(axbuild)* enable x86 kvm acceleration ([#1221](https://github.com/rcore-os/tgoskits/pull/1221))

### Fixed

- *(starry)* reprogram timer for short deadlines ([#1250](https://github.com/rcore-os/tgoskits/pull/1250))
- *(ci)* stabilize x86 Starry QEMU timing ([#1245](https://github.com/rcore-os/tgoskits/pull/1245))
- *(axruntime)* ensure aarch64 SMP IPI readiness before app init ([#1196](https://github.com/rcore-os/tgoskits/pull/1196))

### Other

- *(someboot)* share linker script fragments ([#1218](https://github.com/rcore-os/tgoskits/pull/1218))
- *(ax-net)* unify network stack into single net/ax-net crate, r… ([#1203](https://github.com/rcore-os/tgoskits/pull/1203))

## [0.4.11](https://github.com/rcore-os/tgoskits/compare/axbuild-v0.4.10...axbuild-v0.4.11) - 2026-06-11

### Added

- *(axbuild)* default dynamic platform builds
- *(orangepi-5-plus-uvc-rknn)* add RKNN bench validation ([#1189](https://github.com/rcore-os/tgoskits/pull/1189))
- *(axbuild)* optimize Starry grouped QEMU subcases ([#1201](https://github.com/rcore-os/tgoskits/pull/1201))
- *(axplat-dyn)* add LoongArch64 UEFI dynamic platform ([#1190](https://github.com/rcore-os/tgoskits/pull/1190))

### Fixed

- *(starry)* support eBPF ringbuf mmap on LoongArch DMW ([#1208](https://github.com/rcore-os/tgoskits/pull/1208))
- *(axvisor)* avoid svm guest timer calibration stall ([#1205](https://github.com/rcore-os/tgoskits/pull/1205))
- *(axbuild)* support symlinks in overlay-to-rootfs injection ([#1191](https://github.com/rcore-os/tgoskits/pull/1191))

### Other

- Revert "feat(axplat-dyn): add LoongArch64 UEFI dynamic platform ([#1190](https://github.com/rcore-os/tgoskits/pull/1190))" ([#1202](https://github.com/rcore-os/tgoskits/pull/1202))
- *(axvisor)* remove obsolete x86 q35 static platform ([#1186](https://github.com/rcore-os/tgoskits/pull/1186))

## [0.4.10](https://github.com/rcore-os/tgoskits/compare/axbuild-v0.4.9...axbuild-v0.4.10) - 2026-06-09

### Added

- *(axvisor)* support dynamic x86_64 QEMU guest boot ([#1166](https://github.com/rcore-os/tgoskits/pull/1166))
- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))
- *(starry)* wire qperf app runtime into Starry perf ([#1095](https://github.com/rcore-os/tgoskits/pull/1095))
- *(backtrace)* add showcase workflow ([#1094](https://github.com/rcore-os/tgoskits/pull/1094))
- *(starry-kernel)* support waitid P_PIDFD ([#1051](https://github.com/rcore-os/tgoskits/pull/1051))
- *(axbuild)* improve incremental clippy coverage ([#1088](https://github.com/rcore-os/tgoskits/pull/1088))

### Fixed

- *(axbuild)* tighten incremental clippy selection ([#1183](https://github.com/rcore-os/tgoskits/pull/1183))
- *(axbuild)* infer diff base for zero since ref ([#1143](https://github.com/rcore-os/tgoskits/pull/1143))
- *(ci)* switch x86_64 defaults to dynamic platform ([#1024](https://github.com/rcore-os/tgoskits/pull/1024))

### Other

- *(starry)* add apk curl equivalence system case
- *(starry)* flatten test-suit discovery
- *(starry)* move heavy test workloads to apps
- *(axbuild)* promote image management to top-level command and unify rootfs storage ([#1182](https://github.com/rcore-os/tgoskits/pull/1182))
- *(arceos)* reorganize apps ([#1180](https://github.com/rcore-os/tgoskits/pull/1180))
- *(arceos)* consolidate Rust QEMU test suite ([#1174](https://github.com/rcore-os/tgoskits/pull/1174))
- *(axbuild)* pin ostool runtime bin fix ([#1158](https://github.com/rcore-os/tgoskits/pull/1158))
- *(starry)* add grouped step markers ([#1138](https://github.com/rcore-os/tgoskits/pull/1138))
- Refactor Axvisor to unify ArceOS API and improve modularity ([#1019](https://github.com/rcore-os/tgoskits/pull/1019))

## [0.4.9](https://github.com/rcore-os/tgoskits/compare/axbuild-v0.4.8...axbuild-v0.4.9) - 2026-06-03

### Added

- *(starry-kernel)* port LKM loader + cargo xtask starry kmod build ([#851](https://github.com/rcore-os/tgoskits/pull/851))
- *(axbuild)* support Starry QEMU apps ([#1078](https://github.com/rcore-os/tgoskits/pull/1078))
- *(starry-kernel)* support waitid P_PGID ([#1032](https://github.com/rcore-os/tgoskits/pull/1032))
- *(starryos)* add QEMU K230 boot support ([#1046](https://github.com/rcore-os/tgoskits/pull/1046))
- *(qperf)* TCG hotspot profiling tool for StarryOS ([#940](https://github.com/rcore-os/tgoskits/pull/940))
- *(axtask)* replace PREV_TASK Weak<AxTask> with raw pointer ([#996](https://github.com/rcore-os/tgoskits/pull/996))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))
- *(starry)* add SG2002 board boot support ([#834](https://github.com/rcore-os/tgoskits/pull/834))

### Fixed

- *(axbacktrace)* harden correctness, optimize allocation, and add per-arch IP adjustment ([#1029](https://github.com/rcore-os/tgoskits/pull/1029))
- *(starry)* add loongarch64 to_bin support and rename test case ([#1025](https://github.com/rcore-os/tgoskits/pull/1025))
- *(arceos)* address lockdep test issues ([#1009](https://github.com/rcore-os/tgoskits/pull/1009))
- *(axbuild)* use target spec stem as rustflags config key ([#1023](https://github.com/rcore-os/tgoskits/pull/1023))
- *(starry)* abort test run on first failure ([#983](https://github.com/rcore-os/tgoskits/pull/983))
- *(ci)* stabilize Starry LoongArch apk-curl test ([#959](https://github.com/rcore-os/tgoskits/pull/959))
- *(axbuild)* skip disabled grouped C subcases ([#942](https://github.com/rcore-os/tgoskits/pull/942))
- *(ax-task)* preempt on async wake, guard wait queue against double-enqueue ([#912](https://github.com/rcore-os/tgoskits/pull/912))
- *(starry)* repair SG2002 CI build ([#929](https://github.com/rcore-os/tgoskits/pull/929))
- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))

### Other

- *(platform)* migrate riscv64 qemu to dynamic platform ([#1085](https://github.com/rcore-os/tgoskits/pull/1085))
- *(platform)* remove static aarch64 platforms ([#1074](https://github.com/rcore-os/tgoskits/pull/1074))
- *(linker)* layer platform runtime and final scripts ([#1075](https://github.com/rcore-os/tgoskits/pull/1075))
- *(axvisor)* reorganize VM configs into platform-first directory structure ([#1063](https://github.com/rcore-os/tgoskits/pull/1063))
- *(ci)* bump Rust toolchain to nightly-2026-05-28 and fix clippy ([#1027](https://github.com/rcore-os/tgoskits/pull/1027))
- [AxVisor] add x86_64 UEFI guest support ([#760](https://github.com/rcore-os/tgoskits/pull/760))
- *(starry-kernel)* add memtrack alloc backtrace e2e ([#1020](https://github.com/rcore-os/tgoskits/pull/1020))
- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- Implement platform-specific IRQ handling and architecture setup ([#979](https://github.com/rcore-os/tgoskits/pull/979))
- Adds support for kernel symbol dumping via kallsyms ([#837](https://github.com/rcore-os/tgoskits/pull/837))
- *(starry)* route HAL access through ax-runtime ([#963](https://github.com/rcore-os/tgoskits/pull/963))
- Revert "fix(ax-task): preempt on async wake, guard wait queue against double-…" ([#939](https://github.com/rcore-os/tgoskits/pull/939))
- *(axbuild)* remove unused feature toggles ([#933](https://github.com/rcore-os/tgoskits/pull/933))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))
- *(axbuild)* use target JSON specs for kernel builds ([#839](https://github.com/rcore-os/tgoskits/pull/839))

## [0.4.8](https://github.com/rcore-os/tgoskits/compare/axbuild-v0.4.7...axbuild-v0.4.8) - 2026-05-22

### Added

- *(starry)* add PicoClaw gateway smoke ([#775](https://github.com/rcore-os/tgoskits/pull/775))
- *(axplat-aarch64)* GICv3 + CNTV backend for Apple HVF native execution ([#511](https://github.com/rcore-os/tgoskits/pull/511))
- *(axbuild)* auto symbolize backtrace after ArceOS rust QEMU tests ([#749](https://github.com/rcore-os/tgoskits/pull/749))

### Fixed

- *(repo)* improve rsext4 recovery mount and Axvisor board CI ([#830](https://github.com/rcore-os/tgoskits/pull/830))

### Other

- Revert " fix(repo): improve rsext4 recovery mount and Axvisor board CI ([#830](https://github.com/rcore-os/tgoskits/pull/830))" ([#838](https://github.com/rcore-os/tgoskits/pull/838))
- Remove RISC-V QEMU Virt platform files and update references ([#833](https://github.com/rcore-os/tgoskits/pull/833))
- stream host backtrace symbolize when raw block ends ([#793](https://github.com/rcore-os/tgoskits/pull/793))

## [0.4.7](https://github.com/rcore-os/tgoskits/compare/axbuild-v0.4.6...axbuild-v0.4.7) - 2026-05-19

### Added

- *(git)* enhance global Clippy input handling and fallback logic ([#758](https://github.com/rcore-os/tgoskits/pull/758))

### Other

- Refactor Clippy integration and enhance package handling ([#738](https://github.com/rcore-os/tgoskits/pull/738))

## [0.4.6](https://github.com/rcore-os/tgoskits/compare/axbuild-v0.4.5...axbuild-v0.4.6) - 2026-05-15

### Added

- *(axvisor)* support x86_64(VMX) QEMU guest boot ([#526](https://github.com/rcore-os/tgoskits/pull/526))
- Rust cross-compilation pipeline for StarryOS QEMU test cases ([#471](https://github.com/rcore-os/tgoskits/pull/471))
- *(starryos/procfs)* implement /proc/stat, /proc/cpuinfo, /proc/uptime; fix /proc/meminfo and sysinfo() ([#452](https://github.com/rcore-os/tgoskits/pull/452))
- *(axbuild)* support Starry board examples
- *(realtek-rtl8125)* complete OrangePi board bringup ([#404](https://github.com/rcore-os/tgoskits/pull/404))
- *(ax-net)* add ICMP raw socket support ([#368](https://github.com/rcore-os/tgoskits/pull/368))
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

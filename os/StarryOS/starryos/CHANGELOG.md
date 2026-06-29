# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.19](https://github.com/rcore-os/tgoskits/compare/starryos-v0.5.18...starryos-v0.5.19) - 2026-06-27

### Other

- *(platforms)* remove VisionFive2 static platform ([#1371](https://github.com/rcore-os/tgoskits/pull/1371))

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/starryos-v0.5.17...starryos-v0.5.18) - 2026-06-23

### Other

- updated the following local packages: axplat-dyn, ax-hal, starry-kernel, axbuild, ax-driver, ax-feat, ax-std

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/starryos-v0.5.16...starryos-v0.5.17) - 2026-06-22

### Added

- *(axruntime)* add compiler-backed stack protector support ([#1239](https://github.com/rcore-os/tgoskits/pull/1239))
- AIC8800 Wi-Fi SoftAP for SG2002 (LicheeRV Nano) ([#1185](https://github.com/rcore-os/tgoskits/pull/1185))

### Fixed

- *(tui)* set TERM in init.sh so TUI applications(e.g. top) can start ([#1194](https://github.com/rcore-os/tgoskits/pull/1194))

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/starryos-v0.5.15...starryos-v0.5.16) - 2026-06-12

### Added

- *(starry)* add axbuild kmod support ([#1232](https://github.com/rcore-os/tgoskits/pull/1232))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/starryos-v0.5.14...starryos-v0.5.15) - 2026-06-11

### Fixed

- fix typos in code and comments across the codebase ([#1206](https://github.com/rcore-os/tgoskits/pull/1206))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/starryos-v0.5.13...starryos-v0.5.14) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))
- *(starry)* enable self-compilation on riscv64 with 12GB RAM ([#881](https://github.com/rcore-os/tgoskits/pull/881))

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/starryos-v0.5.12...starryos-v0.5.13) - 2026-06-03

### Added

- *(starryos)* expose K230 KPU device ([#1054](https://github.com/rcore-os/tgoskits/pull/1054))
- *(starry)* add x86_64 self-compilation scripts and documentation ([#973](https://github.com/rcore-os/tgoskits/pull/973))
- *(starryos)* add QEMU K230 boot support ([#1046](https://github.com/rcore-os/tgoskits/pull/1046))
- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))

### Fixed

- *(axbuild)* skip disabled grouped C subcases ([#942](https://github.com/rcore-os/tgoskits/pull/942))

### Other

- *(platform)* remove static aarch64 platforms ([#1074](https://github.com/rcore-os/tgoskits/pull/1074))
- *(linker)* layer platform runtime and final scripts ([#1075](https://github.com/rcore-os/tgoskits/pull/1075))
- *(visual)* add visual-regression test pipeline + Xwayland scenario ([#516](https://github.com/rcore-os/tgoskits/pull/516))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- Refactor code structure for improved readability and maintainability ([#982](https://github.com/rcore-os/tgoskits/pull/982))
- Implement platform-specific IRQ handling and architecture setup ([#979](https://github.com/rcore-os/tgoskits/pull/979))
- Adds support for kernel symbol dumping via kallsyms ([#837](https://github.com/rcore-os/tgoskits/pull/837))
- *(starry)* route HAL access through ax-runtime ([#963](https://github.com/rcore-os/tgoskits/pull/963))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/starryos-v0.5.11...starryos-v0.5.12) - 2026-05-22

### Added

- *(axplat-aarch64)* GICv3 + CNTV backend for Apple HVF native execution ([#511](https://github.com/rcore-os/tgoskits/pull/511))

### Other

- Add kernel tracepoint infrastructure and debugfs integration ([#673](https://github.com/rcore-os/tgoskits/pull/673))

## [0.5.11](https://github.com/rcore-os/tgoskits/compare/starryos-v0.5.10...starryos-v0.5.11) - 2026-05-19

### Other

- updated the following local packages: starry-kernel, axplat-riscv64-visionfive2, axbuild, ax-plat-riscv64-sg2002, axplat-dyn, ax-feat

## [0.5.10](https://github.com/rcore-os/tgoskits/compare/starryos-v0.5.9...starryos-v0.5.10) - 2026-05-15

### Added

- *(starry)* sysfs symlinks + evdev minor base 64 + /run/udev seed for weston ([#508](https://github.com/rcore-os/tgoskits/pull/508))
- *(starry-kernel)* add runtime dynamic debug control ([#446](https://github.com/rcore-os/tgoskits/pull/446))
- *(runtime)* extend IRQ, RTC, and tty event support ([#287](https://github.com/rcore-os/tgoskits/pull/287))

### Fixed

- *(loop)* replace map_or with is_none_or to silence clippy unnecessary_map_or ([#501](https://github.com/rcore-os/tgoskits/pull/501))
- *(arceos)* adjust dynamic platform and network integration
- *(starryos)* restore login shell startup ([#427](https://github.com/rcore-os/tgoskits/pull/427))
- implement close_all_fds function and enhance pipe and syscall handling ([#305](https://github.com/rcore-os/tgoskits/pull/305))

### Other

- Merge pull request #554 from rcore-os/feat/sg2002-pr383
- Enhance build system and add support for RISC-V VisionFive2 platform ([#541](https://github.com/rcore-os/tgoskits/pull/541))
- *(starryos)* inherit workspace metadata
- *(starry)* drop outdated and unmaintained stuffs ([#353](https://github.com/rcore-os/tgoskits/pull/353))

## [0.5.9](https://github.com/rcore-os/tgoskits/compare/starryos-v0.5.8...starryos-v0.5.9) - 2026-04-27

### Other

- Implement RK3588 CRU driver with NPU support and enhancements ([#241](https://github.com/rcore-os/tgoskits/pull/241))

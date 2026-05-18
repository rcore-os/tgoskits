# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

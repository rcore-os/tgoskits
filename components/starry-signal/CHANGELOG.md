# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/starry-signal-v0.7.0...starry-signal-v0.8.0) - 2026-06-02

### Fixed

- *(loongarch64)* make userspace LSX usable (preserve FP/LSX state + fix uc_mcontext offset + advertise AT_HWCAP) ([#917](https://github.com/rcore-os/tgoskits/pull/917))
- *(starry-signal)* keep x86-64 uc_mcontext at Linux ABI offset 40 ([#916](https://github.com/rcore-os/tgoskits/pull/916))

### Other

- *(syscall)* add regression tests for StarryOS signal extension syscalls and fixup ([#806](https://github.com/rcore-os/tgoskits/pull/806))

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/starry-signal-v0.6.2...starry-signal-v0.7.0) - 2026-05-22

### Added

- *(starry)* support multi-threaded execve ([#273](https://github.com/rcore-os/tgoskits/pull/273))

### Other

- *(starry)* add signalfd4 test case, fix ssi_pid/ssi_uid ([#683](https://github.com/rcore-os/tgoskits/pull/683))

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/starry-signal-v0.6.1...starry-signal-v0.6.2) - 2026-05-19

### Other

- updated the following local packages: ax-errno, ax-cpu, starry-vm

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/starry-signal-v0.6.0...starry-signal-v0.6.1) - 2026-05-15

### Added

- *(timer)* implement POSIX timer syscalls (timer_create/settime/gettime/delete ([#341](https://github.com/rcore-os/tgoskits/pull/341))

### Fixed

- *(starryos)* restore login shell startup ([#427](https://github.com/rcore-os/tgoskits/pull/427))

### Other

- *(starry-signal)* fix cross-arch restore assumptions and document prior stack-isolation fix ([#468](https://github.com/rcore-os/tgoskits/pull/468))
- *(starry-signal)* inherit workspace metadata
- update ax-cpu and starry-signal dependencies to version 0.6

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/starry-signal-v0.5.7...starry-signal-v0.6.0) - 2026-04-27

### Added

- *(ax-sync)* add mutex lockdep and fix Starry atomic-context violations ([#271](https://github.com/rcore-os/tgoskits/pull/271))

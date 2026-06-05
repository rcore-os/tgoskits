# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-fs-v0.5.15...ax-fs-v0.5.16) - 2026-06-05

### Other

- updated the following local packages: rsext4, ax-hal

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-fs-v0.5.14...ax-fs-v0.5.15) - 2026-06-03

### Added

- *(axvisor)* support x86_64 Linux guest boot (vmx) ([#930](https://github.com/rcore-os/tgoskits/pull/930))

### Fixed

- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))
- *(rsext4)* rmdir returns ENOTEMPTY on non-empty dirs, rename rejects cross-type overwrites ([#854](https://github.com/rcore-os/tgoskits/pull/854))

### Other

- Refactor journal recovery and partition scanning logic ([#927](https://github.com/rcore-os/tgoskits/pull/927))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-fs-v0.5.13...ax-fs-v0.5.14) - 2026-05-22

### Other

- updated the following local packages: ax-errno, rsext4, ax-hal, ax-fs-vfs, ax-fs-devfs, ax-fs-ramfs, ax-io, ax-driver

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/ax-fs-v0.5.12...ax-fs-v0.5.13) - 2026-05-19

### Other

- updated the following local packages: ax-errno, ax-driver, ax-fs-vfs, ax-fs-devfs, ax-fs-ramfs, ax-io, ax-hal

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-fs-v0.5.11...ax-fs-v0.5.12) - 2026-05-15

### Fixed

- *(arceos)* adjust dynamic platform and network integration

### Other

- *(arceos-modules)* inherit workspace metadata
- *(repo)* split non-USB clippy cleanups ([#372](https://github.com/rcore-os/tgoskits/pull/372))
- *(starry)* drop outdated and unmaintained stuffs ([#353](https://github.com/rcore-os/tgoskits/pull/353))

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.19](https://github.com/rcore-os/tgoskits/compare/ax-sync-v0.5.18...ax-sync-v0.5.19) - 2026-06-12

### Other

- updated the following local packages: ax-task, ax-task

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/ax-sync-v0.5.17...ax-sync-v0.5.18) - 2026-06-11

### Other

- updated the following local packages: ax-task, ax-task

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/ax-sync-v0.5.16...ax-sync-v0.5.17) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-sync-v0.5.15...ax-sync-v0.5.16) - 2026-06-03

### Added

- *(mm)* add page reclaim for file-backed memory pressure (rebased) ([#1007](https://github.com/rcore-os/tgoskits/pull/1007))

### Fixed

- *(axsync)* release raw mutex before waking waiter ([#879](https://github.com/rcore-os/tgoskits/pull/879))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-sync-v0.5.14...ax-sync-v0.5.15) - 2026-05-22

### Other

- updated the following local packages: ax-task, ax-task

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-sync-v0.5.13...ax-sync-v0.5.14) - 2026-05-19

### Other

- updated the following local packages: ax-task, ax-task

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/ax-sync-v0.5.12...ax-sync-v0.5.13) - 2026-05-15

### Added

- *(lockdep)* extend lockdep with task-held tracking and qemu regression coverage ([#415](https://github.com/rcore-os/tgoskits/pull/415))

### Fixed

- *(arceos)* adjust dynamic platform and network integration

### Other

- *(arceos-modules)* inherit workspace metadata

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-sync-v0.5.11...ax-sync-v0.5.12) - 2026-04-27

### Added

- *(ax-sync)* add mutex lockdep and fix Starry atomic-context violations ([#271](https://github.com/rcore-os/tgoskits/pull/271))

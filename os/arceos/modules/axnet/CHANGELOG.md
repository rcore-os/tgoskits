# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/ax-net-v0.5.16...ax-net-v0.5.17) - 2026-06-11

### Other

- updated the following local packages: ax-hal, ax-task, ax-sync, ax-net-ng

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-net-v0.5.15...ax-net-v0.5.16) - 2026-06-09

### Added

- *(ax-posix-api)* improve ArceOS epoll semantics ([#1034](https://github.com/rcore-os/tgoskits/pull/1034))

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-net-v0.5.14...ax-net-v0.5.15) - 2026-06-03

### Fixed

- *(arceos)* address lockdep test issues ([#1009](https://github.com/rcore-os/tgoskits/pull/1009))

### Other

- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-net-v0.5.13...ax-net-v0.5.14) - 2026-05-22

### Other

- updated the following local packages: ax-errno, ax-hal, ax-task, ax-io, ax-driver, ax-sync

## [0.5.13](https://github.com/rcore-os/tgoskits/compare/ax-net-v0.5.12...ax-net-v0.5.13) - 2026-05-19

### Other

- updated the following local packages: ax-errno, ax-driver, ax-task, ax-io, ax-hal, ax-sync

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-net-v0.5.11...ax-net-v0.5.12) - 2026-05-15

### Added

- support sys_setsockopt
- *(net)* migrate ax-net to crates.io smoltcp ([#410](https://github.com/rcore-os/tgoskits/pull/410))

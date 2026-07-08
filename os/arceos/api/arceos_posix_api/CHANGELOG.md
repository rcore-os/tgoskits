# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.28](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.27...ax-posix-api-v0.5.28) - 2026-07-08

### Other

- updated the following local packages: ax-hal, ax-runtime, ax-alloc, ax-task, ax-sync, ax-fs-ng, ax-net

## [0.5.27](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.26...ax-posix-api-v0.5.27) - 2026-07-08

### Other

- updated the following local packages: ax-alloc, ax-hal, ax-task, ax-sync, ax-fs-ng, ax-net, ax-runtime

## [0.5.26](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.25...ax-posix-api-v0.5.26) - 2026-07-08

### Other

- updated the following local packages: ax-kspin, ax-task, ax-alloc, axpoll, ax-hal, ax-sync, ax-fs-ng, ax-log, ax-net, ax-runtime

## [0.5.25](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.24...ax-posix-api-v0.5.25) - 2026-07-07

### Added

- *(starry)* add nix test (no sandbox currently) and kernel regression suite ([#1125](https://github.com/rcore-os/tgoskits/pull/1125))

### Other

- Remove `ax-feat` crate and redistribute features across runtime, API, and user library layers ([#1513](https://github.com/rcore-os/tgoskits/pull/1513))
- remove static platform and axconfig generation, make dynamic platform the only path ([#1478](https://github.com/rcore-os/tgoskits/pull/1478))

## [0.5.24](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.23...ax-posix-api-v0.5.24) - 2026-07-02

### Added

- *(kspin)* add lockdep-aware spin rwlock ([#1397](https://github.com/rcore-os/tgoskits/pull/1397))

## [0.5.23](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.22...ax-posix-api-v0.5.23) - 2026-06-27

### Other

- updated the following local packages: axpoll, scope-local, ax-hal, ax-task, ax-fs-ng, ax-net, ax-runtime, ax-feat, ax-alloc, ax-sync

## [0.5.22](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.21...ax-posix-api-v0.5.22) - 2026-06-23

### Other

- *(ax-net)* add locking and concurrency documentation and remove deprecated interfaces ([#1340](https://github.com/rcore-os/tgoskits/pull/1340))

## [0.5.21](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.20...ax-posix-api-v0.5.21) - 2026-06-22

### Other

- *(arceos)* clean up Hermit remnants ([#1300](https://github.com/rcore-os/tgoskits/pull/1300))
- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.5.20](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.19...ax-posix-api-v0.5.20) - 2026-06-12

### Fixed

- *(axtask)* improve might_sleep diagnostics and coverage ([#1235](https://github.com/rcore-os/tgoskits/pull/1235))

### Other

- *(ax-net)* unify network stack into single net/ax-net crate, r… ([#1203](https://github.com/rcore-os/tgoskits/pull/1203))

## [0.5.19](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.18...ax-posix-api-v0.5.19) - 2026-06-11

### Fixed

- fix typos in code and comments across the codebase ([#1206](https://github.com/rcore-os/tgoskits/pull/1206))

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.17...ax-posix-api-v0.5.18) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))
- *(ax-posix-api)* improve ArceOS epoll semantics ([#1034](https://github.com/rcore-os/tgoskits/pull/1034))

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.16...ax-posix-api-v0.5.17) - 2026-06-03

### Fixed

- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))

### Other

- *(platform)* remove static aarch64 platforms ([#1074](https://github.com/rcore-os/tgoskits/pull/1074))
- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- *(deps)* update spin 0.10→0.12, ostool 0.19→0.21 ([#978](https://github.com/rcore-os/tgoskits/pull/978))
- Refactor workspace structure and update dependencies ([#864](https://github.com/rcore-os/tgoskits/pull/864))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.15...ax-posix-api-v0.5.16) - 2026-05-22

### Other

- updated the following local packages: ax-errno, ax-hal, ax-task, ax-runtime, ax-feat, ax-io, ax-alloc, ax-sync, ax-fs, ax-net

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.14...ax-posix-api-v0.5.15) - 2026-05-19

### Other

- updated the following local packages: ax-errno, ax-alloc, ax-task, ax-io, ax-hal, ax-sync, ax-fs, ax-net, ax-runtime, ax-feat

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.13...ax-posix-api-v0.5.14) - 2026-05-15

### Other

- updated the following local packages: ax-io, scope-local, ax-alloc, ax-config, ax-hal, ax-sync, ax-fs, ax-log, ax-net, ax-task, ax-runtime, ax-feat

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-posix-api-v0.5.11...ax-posix-api-v0.5.12) - 2026-04-27

### Other

- updated the following local packages: ax-alloc

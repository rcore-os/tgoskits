# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.3](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.7.2...ax-api-v0.7.3) - 2026-07-08

### Other

- updated the following local packages: ax-hal, ax-runtime, ax-alloc, ax-ipi, ax-mm, ax-task, ax-sync, ax-display, ax-dma, ax-fs-ng, ax-net

## [0.7.2](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.7.1...ax-api-v0.7.2) - 2026-07-08

### Other

- updated the following local packages: ax-alloc, ax-hal, ax-ipi, ax-mm, ax-task, ax-sync, ax-display, ax-dma, ax-fs-ng, ax-net, ax-runtime

## [0.7.1](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.7.0...ax-api-v0.7.1) - 2026-07-08

### Other

- updated the following local packages: ax-task, ax-alloc, axpoll, ax-hal, ax-ipi, ax-mm, ax-sync, ax-display, ax-dma, ax-fs-ng, ax-log, ax-net, ax-runtime

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.6.4...ax-api-v0.7.0) - 2026-07-07

### Other

- Remove `ax-feat` crate and redistribute features across runtime, API, and user library layers ([#1513](https://github.com/rcore-os/tgoskits/pull/1513))
- remove static platform and axconfig generation, make dynamic platform the only path ([#1478](https://github.com/rcore-os/tgoskits/pull/1478))

## [0.6.4](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.6.3...ax-api-v0.6.4) - 2026-07-02

### Other

- updated the following local packages: ax-errno, ax-hal, ax-ipi, ax-task, ax-display, ax-dma, ax-fs-ng, ax-net, ax-runtime, ax-feat, ax-io, ax-alloc, axpoll, ax-config, ax-mm, ax-sync, ax-log

## [0.6.3](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.6.2...ax-api-v0.6.3) - 2026-06-27

### Other

- updated the following local packages: axpoll, ax-hal, ax-ipi, ax-task, ax-fs-ng, ax-net, ax-runtime, ax-feat, ax-alloc, ax-mm, ax-sync, ax-display, ax-dma

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.6.1...ax-api-v0.6.2) - 2026-06-23

### Other

- *(ax-net)* add locking and concurrency documentation and remove deprecated interfaces ([#1340](https://github.com/rcore-os/tgoskits/pull/1340))

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.6.0...ax-api-v0.6.1) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.5.19...ax-api-v0.6.0) - 2026-06-12

### Fixed

- *(axtask)* improve might_sleep diagnostics and coverage ([#1235](https://github.com/rcore-os/tgoskits/pull/1235))
- *(axtask)* use monotonic deadlines for sleeps ([#1240](https://github.com/rcore-os/tgoskits/pull/1240))

### Other

- *(ax-net)* unify network stack into single net/ax-net crate, r… ([#1203](https://github.com/rcore-os/tgoskits/pull/1203))

## [0.5.19](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.5.18...ax-api-v0.5.19) - 2026-06-11

### Other

- updated the following local packages: ax-alloc, ax-config, ax-hal, ax-mm, ax-task, ax-ipi, ax-sync, ax-display, ax-dma, ax-fs, ax-net, ax-runtime, ax-feat

## [0.5.18](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.5.17...ax-api-v0.5.18) - 2026-06-09

### Added

- *(std)* unify std-aware ArceOS builds ([#1080](https://github.com/rcore-os/tgoskits/pull/1080))

## [0.5.17](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.5.16...ax-api-v0.5.17) - 2026-06-03

### Added

- *(axtask)* add task stack guard page support ([#811](https://github.com/rcore-os/tgoskits/pull/811))

### Other

- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))
- Refactor workspace structure and update dependencies ([#864](https://github.com/rcore-os/tgoskits/pull/864))

## [0.5.16](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.5.15...ax-api-v0.5.16) - 2026-05-22

### Other

- updated the following local packages: ax-errno, ax-hal, ax-task, ax-runtime, ax-feat, ax-io, ax-alloc, ax-mm, ax-dma, ax-driver, ax-sync, ax-display, ax-fs, ax-ipi, ax-net

## [0.5.15](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.5.14...ax-api-v0.5.15) - 2026-05-19

### Other

- updated the following local packages: ax-errno, ax-alloc, ax-driver, ax-task, ax-io, ax-hal, ax-mm, ax-dma, ax-sync, ax-display, ax-fs, ax-ipi, ax-net, ax-runtime, ax-feat

## [0.5.14](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.5.13...ax-api-v0.5.14) - 2026-05-15

### Other

- updated the following local packages: ax-io, ax-alloc, ax-config, ax-hal, ax-mm, ax-sync, ax-fs, ax-log, ax-net, ax-dma, ax-driver, ax-task, ax-display, ax-ipi, ax-runtime, ax-feat

## [0.5.12](https://github.com/rcore-os/tgoskits/compare/ax-api-v0.5.11...ax-api-v0.5.12) - 2026-04-27

### Other

- updated the following local packages: ax-alloc

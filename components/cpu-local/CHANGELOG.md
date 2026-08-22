# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/cpu-local-v0.1.2...cpu-local-v0.2.0) - 2026-08-20

### Added

- *(percpu)* add scheduler-owned CPU access ([#2081](https://github.com/rcore-os/tgoskits/pull/2081))

### Fixed

- *(cpu-local)* keep AArch64 current independent of TLS ([#1970](https://github.com/rcore-os/tgoskits/pull/1970))

### Other

- *(axtest)* standardize Cargo and QEMU test flow ([#2088](https://github.com/rcore-os/tgoskits/pull/2088))
- *(cpu-local)* define scheduler-neutral execution context boundary ([#2080](https://github.com/rcore-os/tgoskits/pull/2080))

### Added

- Add a non-escaping current-CPU area capability for future low-level owners;
  there is no runtime caller yet.

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/cpu-local-v0.1.1...cpu-local-v0.1.2) - 2026-08-03

### Fixed

- *(cpu-local)* guard uninstalled host CPU areas ([#1798](https://github.com/rcore-os/tgoskits/pull/1798))

## [0.1.1](https://github.com/rcore-os/tgoskits/compare/cpu-local-v0.1.0...cpu-local-v0.1.1) - 2026-07-23

### Other

- *(ax-runtime)* centralize UART scheduling ([#1675](https://github.com/rcore-os/tgoskits/pull/1675))

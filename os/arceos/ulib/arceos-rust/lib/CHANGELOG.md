# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.1.1](https://github.com/rcore-os/tgoskits/compare/arceos-rust-interface-v1.1.0...arceos-rust-interface-v1.1.1) - 2026-06-05

### Other

- updated the following local packages: ax-driver, ax-hal, ax-runtime, ax-feat, ax-api, ax-posix-api

## [1.1.0](https://github.com/rcore-os/tgoskits/compare/arceos-rust-interface-v1.0.3...arceos-rust-interface-v1.1.0) - 2026-06-03

### Added

- *(riscv64)* support dynamic platform on QEMU and SG2002 ([#961](https://github.com/rcore-os/tgoskits/pull/961))
- *(axtask)* add task stack guard page support ([#811](https://github.com/rcore-os/tgoskits/pull/811))

### Fixed

- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))

### Other

- *(platform)* remove static aarch64 platforms ([#1074](https://github.com/rcore-os/tgoskits/pull/1074))
- *(ax-alloc)* remove ax-allocator dependency, simplify to TLSF/buddy-slab backends ([#987](https://github.com/rcore-os/tgoskits/pull/987))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))

## [1.0.3](https://github.com/rcore-os/tgoskits/compare/arceos-rust-interface-v1.0.2...arceos-rust-interface-v1.0.3) - 2026-05-22

### Other

- updated the following local packages: ax-errno, ax-runtime, ax-feat, ax-io, ax-api, ax-posix-api

## [1.0.2](https://github.com/rcore-os/tgoskits/compare/arceos-rust-interface-v1.0.1...arceos-rust-interface-v1.0.2) - 2026-05-19

### Other

- updated the following local packages: ax-errno, ax-io, ax-runtime, ax-feat, ax-api, ax-posix-api

## [1.0.1](https://github.com/rcore-os/tgoskits/compare/arceos-rust-interface-v1.0.0...arceos-rust-interface-v1.0.1) - 2026-05-15

### Other

- updated the following local packages: ax-kspin, ax-io, ax-runtime, ax-feat, ax-api, ax-posix-api

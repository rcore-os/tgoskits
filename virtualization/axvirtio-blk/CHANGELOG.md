# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/rcore-os/tgoskits/compare/axvirtio-blk-v0.2.1...axvirtio-blk-v0.3.0) - 2026-09-11

### Added

- *(virtio)* add synchronized VirtIO block PCI ramdisk ([#2070](https://github.com/rcore-os/tgoskits/pull/2070))

## [0.2.1](https://github.com/rcore-os/tgoskits/compare/axvirtio-blk-v0.2.0...axvirtio-blk-v0.2.1) - 2026-09-09

### Added

- *(axvm)* service virtio block images with on-demand file I/O ([#2310](https://github.com/rcore-os/tgoskits/pull/2310))
- *(virtio)* implement split-ring event index ([#2255](https://github.com/rcore-os/tgoskits/pull/2255))

### Fixed

- *(repo)* remove redundant Cargo manifest declarations ([#2297](https://github.com/rcore-os/tgoskits/pull/2297))

### Other

- *(repo)* remove duplicated tests and configuration snapshots ([#2326](https://github.com/rcore-os/tgoskits/pull/2326))
- *(repo)* continue removing nonfunctional test scaffolding ([#2307](https://github.com/rcore-os/tgoskits/pull/2307))

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/axvirtio-blk-v0.1.0...axvirtio-blk-v0.2.0) - 2026-08-20

### Added

- *(axvirtio-blk)* add virtio-mmio block device core ([#1935](https://github.com/rcore-os/tgoskits/pull/1935))

### Fixed

- *(axdevice)* [**breaking**] bind device access to the issuing vCPU ([#2092](https://github.com/rcore-os/tgoskits/pull/2092))
- *(axvirtio-common)* harden shared virtqueue against untrusted guests ([#1984](https://github.com/rcore-os/tgoskits/pull/1984))

### Other

- *(axtest)* standardize Cargo and QEMU test flow ([#2088](https://github.com/rcore-os/tgoskits/pull/2088))

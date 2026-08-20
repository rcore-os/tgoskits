# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/axvirtio-net-v0.1.0...axvirtio-net-v0.2.0) - 2026-08-20

### Fixed

- *(axdevice)* [**breaking**] bind device access to the issuing vCPU ([#2092](https://github.com/rcore-os/tgoskits/pull/2092))
- *(axvirtio-common)* harden shared virtqueue against untrusted guests ([#1984](https://github.com/rcore-os/tgoskits/pull/1984))

### Other

- *(axtest)* standardize Cargo and QEMU test flow ([#2088](https://github.com/rcore-os/tgoskits/pull/2088))
- *(sync)* unify lock primitives in ax-sync ([#1956](https://github.com/rcore-os/tgoskits/pull/1956))

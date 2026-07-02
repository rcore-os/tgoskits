# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.2.0...sdmmc-protocol-v0.3.0) - 2026-07-02

### Fixed

- *(ci)* prevent Starry qemu hangs in IRQ paths ([#1431](https://github.com/rcore-os/tgoskits/pull/1431))

### Other

- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.1.3...sdmmc-protocol-v0.2.0) - 2026-06-27

### Added

- *(sdmmc)* implement native host2 RDIF path

## [0.1.3](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.1.2...sdmmc-protocol-v0.1.3) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.1.1...sdmmc-protocol-v0.1.2) - 2026-06-03

### Other

- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))

## [0.1.0](https://github.com/rcore-os/tgoskits/releases/tag/sdmmc-protocol-v0.1.0) - 2026-05-16

### Added

- *(sdmmc)* add reusable SD/MMC protocol and host drivers ([#538](https://github.com/rcore-os/tgoskits/pull/538))

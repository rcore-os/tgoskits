# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.11.3](https://github.com/rcore-os/tgoskits/compare/rdif-block-v0.11.2...rdif-block-v0.11.3) - 2026-08-03

### Added

- *(ahci-driver)* add portable multi-disk AHCI support ([#1795](https://github.com/rcore-os/tgoskits/pull/1795))

### Fixed

- *(dma-api)* retire legacy axdma release paths ([#1796](https://github.com/rcore-os/tgoskits/pull/1796))

### Other

- *(block)* adopt IRQ-driven multi-queue runtime ([#1768](https://github.com/rcore-os/tgoskits/pull/1768))
- enhance axtest coverage for various starry-kernel contracts ([#1674](https://github.com/rcore-os/tgoskits/pull/1674))

### Changed

- Add runtime-owned register retry delays for controller and queue state
  machines without permitting completion polling.
- Allow runtimes to restore an unaccepted request suffix from an owned batch
  without reallocating its submission container.
- Clarify that controller shutdown must complete before queue-owned DMA memory
  can be released.
- Add ordered DMA-owning request batches with explicit partial acceptance and accepted-ID publication.
- Require a separate queue commit after every non-empty batch so native drivers can publish multiple descriptors with one hardware doorbell.
- Add `max_submit_batch` to the mandatory hardware queue limits.
- Replace submit/poll queues with DMA-owning `BlockController`, `HardwareQueue`, and boxed hard-IRQ acknowledgement contracts.
- Make DMA masks, alignments, segment limits, boundaries, and queue depth explicit and mandatory at both planning and submission boundaries.

### Removed

- Remove legacy queue handles, completion polling, cancellation/status APIs, and software-only request flags.

## [0.11.2](https://github.com/rcore-os/tgoskits/compare/rdif-block-v0.11.1...rdif-block-v0.11.2) - 2026-07-08

### Other

- updated the following local packages: ax-kspin, dma-api

## [0.11.1](https://github.com/rcore-os/tgoskits/compare/rdif-block-v0.11.0...rdif-block-v0.11.1) - 2026-07-07

### Other

- updated the following local packages: ax-kspin, dma-api

## [0.11.0](https://github.com/rcore-os/tgoskits/compare/rdif-block-v0.10.0...rdif-block-v0.11.0) - 2026-07-02

### Fixed

- *(ci)* prevent Starry qemu hangs in IRQ paths ([#1431](https://github.com/rcore-os/tgoskits/pull/1431))

### Other

- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))

## [0.10.0](https://github.com/rcore-os/tgoskits/compare/rdif-block-v0.9.1...rdif-block-v0.10.0) - 2026-06-27

### Added

- *(rdif-block)* add owned DMA queue primitives

### Fixed

- *(locking)* remove spin mutex usage from kernel paths ([#1380](https://github.com/rcore-os/tgoskits/pull/1380))

## [0.9.1](https://github.com/rcore-os/tgoskits/compare/rdif-block-v0.9.0...rdif-block-v0.9.1) - 2026-06-23

### Other

- updated the following local packages: dma-api

## [0.9.0](https://github.com/rcore-os/tgoskits/compare/rdif-block-v0.8.2...rdif-block-v0.9.0) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.8.2](https://github.com/rcore-os/tgoskits/compare/rdif-block-v0.8.1...rdif-block-v0.8.2) - 2026-06-12

### Other

- updated the following local packages: rdif-base

## [0.8.1](https://github.com/rcore-os/tgoskits/compare/rdif-block-v0.8.0...rdif-block-v0.8.1) - 2026-06-09

### Other

- updated the following local packages: rdif-base, dma-api

## [0.8.0](https://github.com/rcore-os/tgoskits/compare/rdif-block-v0.7.1...rdif-block-v0.8.0) - 2026-06-03

### Other

- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))

## [0.6.1](https://github.com/drivercraft/rdrive/compare/rdif-block-v0.6.0...rdif-block-v0.6.1) - 2025-09-23

### Other

- rdrive rm deps

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Establish the MMC default-speed clock before optional HS52/HS200 selection,
  and publish the proven bus width and clock mode with the initialized card.
- Replace the RDIF shared-card core's unbounded atomic spin with one-shot
  mutable borrowing. Contended submits now return `Retry` with exact request
  ownership, while IRQ/lifecycle workers retain their event and return typed
  deferred progress for a later bounded pass.

### Changed

- Remove the unused synchronous SPI block backend. Hardware block I/O is now
  exposed only through the native host/RDIF path, where acknowledged IRQ events
  advance normal requests and absolute deadlines are limited to initialization
  and watchdog failure.
- Require a move-only `InitializedSdioCard` capability from the terminal init
  state before constructing an RDIF block device; raw cards can no longer be
  promoted directly into a Ready interface.
- Retain controller-owned clock/reset capabilities across the Ready transition
  so recovery and guest-return reconstruction keep their hardware resources.
- Add a typed, non-blocking interrupt-controller lifecycle and defer active
  request/DMA reclamation until the host proves controller quiescence.
- Route deferred controller IRQs during recovery and reinitialization through
  the host's destructive-ack endpoint before exposing them to lifecycle state,
  and fail recovery immediately if that owned-source acknowledgement fails.
- Distinguish a contended retry, a non-empty acknowledged hardware snapshot,
  and a retry that finds no device source; an empty retry no longer advances
  initialization, recovery, or request completion.
- Resolve a deferred level source before checking queue request state. A
  non-empty snapshot that cannot be bound to the active request now enters
  typed recovery instead of leaving the device source asserted or silently
  discarding generation-ambiguous state.
- Preallocate optional Host2 recovery storage during activation so runtime
  recovery does not allocate.
- Replace poll-count card initialization with explicit controller-IRQ and
  absolute monotonic schedules, and add pinned `OwnedSdioInit` plus the RDIF
  `StagedBlockDevice` discovery-to-ready adapter.
- Add `poll_bus_op_at` and the opt-in timed host2 adapter so eventless platform
  transitions publish their exact absolute activation without a global clock.
- Require every SDIO host to implement IRQ enable/disable state explicitly;
  runtime queues no longer inherit a silent no-op IRQ capability.
- Keep runtime abort failures ownership-preserving: a host that has transferred
  completion status to IRQ context must return `Busy` until its controller
  lifecycle proves DMA quiescence.
- Let IRQ endpoints distinguish acknowledged controller sideband events from
  events that can advance the serialized block queue, while retaining the
  conservative queue-service default for existing hosts.
- Add an explicit interrupt-PIO data path alongside DMA and initialization-only
  FIFO access. The RDIF queue now preserves exact CPU-buffer ownership across
  submit rejection, IRQ completion, and proof-gated recovery in both modes.

## [0.4.1](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.4.0...sdmmc-protocol-v0.4.1) - 2026-07-08

### Other

- updated the following local packages: dma-api, sdio-host2, rdif-block

## [0.4.0](https://github.com/rcore-os/tgoskits/compare/sdmmc-protocol-v0.3.0...sdmmc-protocol-v0.4.0) - 2026-07-07

### Added

- *(cv181x-sdhci)* add SG2002 SD driver ([#1482](https://github.com/rcore-os/tgoskits/pull/1482))

### Other

- *(sdmmc-protocol)* split SDIO and RDIF capability modules ([#1486](https://github.com/rcore-os/tgoskits/pull/1486))

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

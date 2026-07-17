# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Require the protocol's initialized-card capability before the RDIF adapter
  can construct its interrupt-backed runtime device.
- Migrate the RDIF adapter to owned IRQ-only queue semantics and drain combined
  command/data events without task-side completion polling.
- Replace the task-side register spin gate with a non-blocking `Busy` result,
  cap FIFO IRQ continuations at 64 words, and keep error ownership until typed
  controller quiescence.
- Drive reset/clock bus states from absolute monotonic time and reject runtime
  aborts until lifecycle quiescence, preventing worker context from entering a
  synchronous reset or returning DMA ownership early.
- Reject owned CPU PIO buffers without consuming their transaction, so callers
  can recover the exact allocation when this DMA-only runtime path is selected.
- Separate IDMAC descriptor completion, controller transfer completion, and
  R1b busy release so no partial hardware event can publish a terminal request.
- Make post-IDMAC command activation infallible and delay conversion to
  `InFlightDma` until admission succeeds, preventing either reuse or quarantine
  of backing that hardware never accepted.
- Reject initialization before completion IRQ delivery is bound, serialize
  reset cleanup against the IRQ endpoint, and mask both controller and IDMAC
  sources before recovery.

## [0.3.2](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.3.1...phytium-mci-host-v0.3.2) - 2026-07-08

### Other

- updated the following local packages: dma-api, sdio-host2, rdif-block, sdmmc-protocol

## [0.3.1](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.3.0...phytium-mci-host-v0.3.1) - 2026-07-07

### Other

- *(sdmmc-protocol)* split SDIO and RDIF capability modules ([#1486](https://github.com/rcore-os/tgoskits/pull/1486))

## [0.3.0](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.2.0...phytium-mci-host-v0.3.0) - 2026-07-02

### Other

- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.1.5...phytium-mci-host-v0.2.0) - 2026-06-27

### Added

- *(sdmmc)* implement native host2 RDIF path

## [0.1.5](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.1.4...phytium-mci-host-v0.1.5) - 2026-06-23

### Other

- updated the following local packages: dma-api

## [0.1.4](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.1.3...phytium-mci-host-v0.1.4) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.1.3](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.1.2...phytium-mci-host-v0.1.3) - 2026-06-09

### Other

- updated the following local packages: dma-api

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/phytium-mci-host-v0.1.1...phytium-mci-host-v0.1.2) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))

### Other

- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(dma-api)* split coherent and streaming DMA APIs ([#932](https://github.com/rcore-os/tgoskits/pull/932))

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Consume the protocol's initialized-card capability when publishing RDIF and
  retain platform clock capabilities through recovery and ownership handoff.
- Migrate the RDIF adapter to owned IRQ-only queue semantics and prevent task
  context from reading or acknowledging runtime completion status.
- Add bounded controller/FIFO/IDMAC reset and clock reconstruction states with
  absolute wake deadlines and proof-gated DMA reclamation.
- Fail closed the direct `SdioHost` bus-operation compatibility path so card
  initialization cannot enter legacy synchronous reset/clock helpers.
- Replace the task-side register spin gate with a non-blocking `Busy` result
  and cap each FIFO IRQ continuation at 64 words.
- Use caller-supplied absolute monotonic deadlines for reset/clock bus states;
  runtime aborts now retain request and DMA ownership until typed lifecycle
  quiescence instead of entering a synchronous controller reset.
- Reject the shared host2 owned-CPU PIO variant without consuming its backing;
  DWMMC runtime queues remain explicitly IDMAC-only.
- Keep IDMAC `RI`/`TI` completion separate from controller `DATA_OVER`, require
  both generation-tagged events for DMA success, and let either error source win
  over a combined completion snapshot while preserving exact IDMAC diagnostics.
- Remove the post-admission card-detect failure window: once IDMAC owns the
  request buffer, command activation is infallible and later card removal is
  handled by IRQ/watchdog recovery.
- Move bounce-buffer in-flight conversion to the hardware commit point, and
  quarantine both accepted data buffers and IDMAC descriptor tables until
  terminal completion or reset-derived quiescence permits release.
- Add explicit DMA/device ordering barriers before IDMAC fetch, command
  activation, and IRQ mailbox publication for weakly ordered architectures.
- Mask the controller interrupt output during discovery without issuing a
  command or acknowledging status, and discard stale IDMAC status only after
  reset has established typed lifecycle quiescence.

## [0.3.2](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.3.1...dwmmc-host-v0.3.2) - 2026-07-08

### Other

- updated the following local packages: dma-api, sdio-host2, rdif-block, sdmmc-protocol

## [0.3.1](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.3.0...dwmmc-host-v0.3.1) - 2026-07-07

### Added

- *(starfive-jh7110-dwmmc)* add IRQ-driven host ([#1524](https://github.com/rcore-os/tgoskits/pull/1524))

### Other

- *(sdmmc-protocol)* split SDIO and RDIF capability modules ([#1486](https://github.com/rcore-os/tgoskits/pull/1486))

## [0.3.0](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.2.0...dwmmc-host-v0.3.0) - 2026-07-02

### Other

- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.1.5...dwmmc-host-v0.2.0) - 2026-06-27

### Added

- *(sdmmc)* implement native host2 RDIF path

## [0.1.5](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.1.4...dwmmc-host-v0.1.5) - 2026-06-23

### Other

- updated the following local packages: dma-api

## [0.1.4](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.1.3...dwmmc-host-v0.1.4) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.1.3](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.1.2...dwmmc-host-v0.1.3) - 2026-06-09

### Other

- updated the following local packages: dma-api

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/dwmmc-host-v0.1.1...dwmmc-host-v0.1.2) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))

### Other

- *(dma-api)* split coherent and streaming DMA APIs ([#932](https://github.com/rcore-os/tgoskits/pull/932))

## [0.1.0](https://github.com/rcore-os/tgoskits/releases/tag/dwmmc-host-v0.1.0) - 2026-05-16

### Added

- *(sdmmc)* add reusable SD/MMC protocol and host drivers ([#538](https://github.com/rcore-os/tgoskits/pull/538))

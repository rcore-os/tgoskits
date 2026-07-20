# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Quarantine an active ADMA descriptor table when its request is dropped
  without terminal IRQ evidence or controller quiescence; successful and
  proof-gated completion release the table exactly at the ownership boundary.
- Consume an acknowledged ADMA boundary indication as part of the active data
  snapshot, including when it is coalesced with transfer completion, so a
  multi-block request can hand off directly to its explicit CMD12 IRQ epoch.
- Acknowledge card/retuning/vendor sideband status without publishing it into
  the active request generation or scheduling block-queue service.

### Changed

- Replace the repeatable IRQ handle with an exclusive live `SdhciIrqSource` lease:
  the hard-IRQ endpoint exclusively captures and W1C-acknowledges status, while
  the fixed maintenance owner retains a separate generation-checked rearm
  capability. Controller delivery cannot be enabled through the public
  protocol boundary before the source transfers to OS glue; after explicit
  mask/synchronize teardown, dropping both lease halves permits a later
  initialization or runtime generation to acquire the source again.
- Make every low-level raw-pointer block submission API `unsafe` and document
  the cross-worker lifetime/exclusive-access contract. Safe protocol and RDIF
  paths continue to retain either the Rust borrow or the owned DMA/CPU buffer.
- Expose the effective bus clock proven by the completed clock state machine,
  including platform-quantized external clock rates.
- Consume the protocol's initialized-card capability when publishing RDIF and
  retain platform clock/reset capabilities through recovery and ownership
  handoff.
- Add bounded SDHCI reset/reconstruction states with absolute wake deadlines
  and proof-gated ADMA reclamation.
- Require platform reset hooks to explicitly declare bounded support for both
  initial ResetAll and recovery; an unproven hook fails before callback or MMIO
  reset side effects.
- Fail closed the direct `SdioHost` bus-operation/tuning compatibility path;
  staged initialization must use the native host2 state machines.
- Add a typed scheduled ResetAll hook with begin/poll/cancel transitions and
  absolute deadlines for platform reset pulses during init and recovery.
- Separate initialization-owned status access from runtime IRQ ownership;
  masked runtime FIFO and R1b paths no longer use present-state or W1C polling
  to synthesize completion, and error snapshots defer reset to recovery.
- Reject an IRQ-owned submission while the command/data engine is inhibited,
  and publish a new ADMA address only in the final command-issue step, so
  watchdog activation cannot become an eventless retry path or expose
  caller-owned descriptors to busy hardware.
- Make request-generation handoff conditional on an empty IRQ mailbox; pending
  evidence blocks the next command, while a late event carrying the previous
  generation is ignored instead of completing the new request.
- Drive reset, clock, voltage, and tuning transitions from caller-supplied
  absolute monotonic time, and remove the synchronous clock-programming API.
- Rename normal block progression to `service_block_request`; completion can
  only be advanced by an IRQ snapshot, while watchdog expiry only fails and
  quarantines the request for lifecycle recovery.
- Add an explicit owned interrupt-PIO runtime configuration for FIFO-only
  SDHCI integrations. Submit failure, IRQ completion, and recovery return the
  exact CPU buffer without introducing a completion-poll fallback; a final
  buffer-ready event coalesced with transfer-complete is consumed in the same
  bounded service pass.

## [0.4.1](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.4.0...sdhci-host-v0.4.1) - 2026-07-08

### Other

- updated the following local packages: dma-api, sdio-host2, rdif-block, sdmmc-protocol

## [0.4.0](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.3.0...sdhci-host-v0.4.0) - 2026-07-07

### Other

- *(drivers)* split Rockchip reset capability ([#1509](https://github.com/rcore-os/tgoskits/pull/1509))
- *(sdmmc-protocol)* split SDIO and RDIF capability modules ([#1486](https://github.com/rcore-os/tgoskits/pull/1486))

## [0.3.0](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.2.0...sdhci-host-v0.3.0) - 2026-07-02

### Other

- *(rdif-block)* enable boxed sdmmc irq flow ([#1446](https://github.com/rcore-os/tgoskits/pull/1446))

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.1.5...sdhci-host-v0.2.0) - 2026-06-27

### Added

- *(sdmmc)* implement native host2 RDIF path

## [0.1.5](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.1.4...sdhci-host-v0.1.5) - 2026-06-23

### Other

- updated the following local packages: dma-api

## [0.1.4](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.1.3...sdhci-host-v0.1.4) - 2026-06-22

### Fixed

- *(sdhci-host)* preserve fifo irq error status ([#1291](https://github.com/rcore-os/tgoskits/pull/1291))

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.1.3](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.1.2...sdhci-host-v0.1.3) - 2026-06-09

### Other

- updated the following local packages: dma-api

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/sdhci-host-v0.1.1...sdhci-host-v0.1.2) - 2026-06-03

### Added

- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))

### Other

- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(dma-api)* split coherent and streaming DMA APIs ([#932](https://github.com/rcore-os/tgoskits/pull/932))

## [0.1.0](https://github.com/rcore-os/tgoskits/releases/tag/sdhci-host-v0.1.0) - 2026-05-16

### Added

- *(sdmmc)* add reusable SD/MMC protocol and host drivers ([#538](https://github.com/rcore-os/tgoskits/pull/538))

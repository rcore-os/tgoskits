# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Transparently transfer SDHCI's one-shot capture/control IRQ source instead
  of exposing a repeatable handler. Completion delivery now fails closed until
  OS glue retains both capabilities on the fixed maintenance owner.
- Require the protocol's initialized-card capability before the RDIF adapter
  can construct its interrupt-backed runtime device.
- Migrate the RDIF adapter to the 0.12 owned interrupt contract, delegate ADMA
  queue constraints and typed recovery to SDHCI, and expose timed
  initialization for runtime-driven absolute deadlines.
- Keep discovery side-effect free by deferring CV181x PHY programming until
  the IRQ-bound ResetAll/PowerOn initialization state machine runs.
- Preserve board registers when a PowerOff or 3.3-V transition is rejected by
  admitting the bus operation before applying its platform-side transition.
- Keep raw SDHCI and board-register mutation private so callers cannot bypass
  staged IRQ ownership, initialization, or recovery.
- Represent the mapped controller/syscon pair as a move-only capability and
  concentrate its pointer, alignment, cross-CPU, and lifetime proof at unsafe
  construction; host construction is safe once that capability exists.

## [0.1.2](https://github.com/rcore-os/tgoskits/compare/cv181x-sdhci-v0.1.1...cv181x-sdhci-v0.1.2) - 2026-07-08

### Other

- updated the following local packages: dma-api, sdio-host2, rdif-block, sdmmc-protocol, sdhci-host

## [0.1.1](https://github.com/rcore-os/tgoskits/compare/cv181x-sdhci-v0.1.0...cv181x-sdhci-v0.1.1) - 2026-07-07

### Other

- *(sdmmc-protocol)* split SDIO and RDIF capability modules ([#1486](https://github.com/rcore-os/tgoskits/pull/1486))

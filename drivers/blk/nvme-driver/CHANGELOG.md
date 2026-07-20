# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Migrate block queues to the IRQ-driven rdif-block 0.12 owned-request contract,
  with bounded completion draining and explicit DMA ownership return.
- Add a non-blocking typed controller lifecycle that waits for CC.RDY with
  absolute deadlines and lets the maintenance owner rebuild retained queues
  only from IRQ-evidenced admin completions after recovery.
- Split lifecycle IRQ ownership into a hard-IRQ capture endpoint and a
  generation-checked owner-side rearm endpoint.
- Restrict the hard-IRQ endpoint to vector masking and immutable queue routing;
  the CPU-pinned maintenance owner now exclusively consumes admin and I/O CQs,
  rings doorbells, and publishes terminal request ownership.
- Make PCI discovery command-free and move reset, Identify, queue creation, and
  namespace publication into an IRQ-bound initialization state machine.
- Remove synchronous command and block-I/O polling APIs, global command IDs,
  and the unused spin dependency; preserve 64-bit namespace capacity and honor
  controller page-size and queue-depth capabilities.

### Fixed

- Bind queue DMA reclaim to the controller cookie and a strictly advancing
  lifecycle epoch, and report invalid proofs separately from request errors.
- Keep a cached CQ slot published until the maintenance owner has copied its complete
  payload, preventing a late or duplicate CQE from overwriting the result
  concurrently being consumed.
- Accept bidirectional DMA buffers for read and write operations, matching the
  direction contract already validated by rdif-block.
- Disable the controller and wait for `CC.RDY=0` before publishing an
  initialization, namespace-publication, or reinitialization failure,
  retaining DMA state in quarantine when the abort deadline expires.
- Require an acknowledged `QueueEventBatch` before owner-side code reads an
  I/O CQ, so a cache-only call cannot turn into completion polling after the
  corresponding source event has been consumed.
- Preserve every queue routed to one shared source in its stable event, without
  reading or advancing any CQ from hard IRQ.
- Freeze the logical-device and per-queue tag depth from the usable common
  SQ/CQ capacity, including the reserved ring entry, instead of advertising
  the larger requested depth.
- Refuse to touch controller registers during initialization until both the
  admin IRQ capture endpoint and its delivery path are live.
- Reject vector mappings outside the controller's 32-bit INTMS/INTMC range so
  every published IRQ source remains device-maskable during quiesce.
- Reject queue topologies larger than the fixed RDIF queue-event mask instead
  of initializing hardware queues that the runtime can never publish.
- Require the first published I/O queue to share MSI-X vector zero with the
  admin CQ, preserving a permanent recovery IRQ route even when fewer queues
  are materialized than the controller topology preallocated.
- Observe a new CQ phase before the read barrier and reload the CQE afterward,
  preserving device-to-CPU ordering for completion fields and DMA data on weak
  memory-order architectures; publish the CQ head only after those reads
  retire.
- Validate a bounded snapshot of every cached CID before publishing any
  terminal completion, so a stale or structurally invalid CQE enters recovery
  before callbacks can expose partial success from the same service batch.

## [0.7.2](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.7.1...nvme-driver-v0.7.2) - 2026-07-10

### Added

- *(msi)* add hierarchical MSI-X irq domains ([#1526](https://github.com/rcore-os/tgoskits/pull/1526))

## [0.7.1](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.7.0...nvme-driver-v0.7.1) - 2026-07-08

### Other

- updated the following local packages: dma-api, rdif-block

## [0.7.0](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.6.3...nvme-driver-v0.7.0) - 2026-07-07

### Added

- *(msi)* add aarch64 MSI-X registration ([#1522](https://github.com/rcore-os/tgoskits/pull/1522))

## [0.6.3](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.6.2...nvme-driver-v0.6.3) - 2026-07-02

### Fixed

- *(ci)* prevent Starry qemu hangs in IRQ paths ([#1431](https://github.com/rcore-os/tgoskits/pull/1431))

## [0.6.2](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.6.1...nvme-driver-v0.6.2) - 2026-06-27

### Added

- *(rdif-block)* add owned DMA queue primitives

### Other

- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.6.1](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.6.0...nvme-driver-v0.6.1) - 2026-06-23

### Other

- updated the following local packages: dma-api, rdif-block

## [0.6.0](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.5.2...nvme-driver-v0.6.0) - 2026-06-22

### Other

- *(ax-runtime)* adapt submit-poll fs block irq registration ([#1228](https://github.com/rcore-os/tgoskits/pull/1228))

## [0.5.2](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.5.1...nvme-driver-v0.5.2) - 2026-06-12

### Other

- updated the following local packages: rdif-block

## [0.5.1](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.5.0...nvme-driver-v0.5.1) - 2026-06-09

### Other

- updated the following local packages: pcie, dma-api, rdif-block

## [0.5.0](https://github.com/rcore-os/tgoskits/compare/nvme-driver-v0.4.2...nvme-driver-v0.5.0) - 2026-06-03

### Added

- *(axbuild)* support Starry QEMU apps ([#1078](https://github.com/rcore-os/tgoskits/pull/1078))
- *(dma-api)* add high-level dma sync helpers ([#1028](https://github.com/rcore-os/tgoskits/pull/1028))

### Fixed

- *(repo)* migrate spin usage to ax-kspin ([#861](https://github.com/rcore-os/tgoskits/pull/861))

### Other

- *(rdif-block)* switch block drivers to submit poll ([#976](https://github.com/rcore-os/tgoskits/pull/976))
- *(dma-api)* split coherent and streaming DMA APIs ([#932](https://github.com/rcore-os/tgoskits/pull/932))
- *(drivers)* split shared driver stack from ArceOS ([#831](https://github.com/rcore-os/tgoskits/pull/831))

## [0.4.1](https://github.com/drivercraft/sparreal-os/compare/nvme-driver-v0.4.0...nvme-driver-v0.4.1) - 2026-03-10

### Other

- ✨ feat: 更新 fdt-edit 和 fdt-raw 版本，优化 FDT 相关功能 ([#47](https://github.com/drivercraft/sparreal-os/pull/47))
- ♻️ refactor(PCIe): PCIe driver use mmio_api for memory-mapped I/O operations

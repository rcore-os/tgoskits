# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Bind every interrupt `QueueHandle` to its retained controller and publication
  epoch, then reject foreign, pre-publication, or replayed DMA-quiescence proofs
  in the generic RDIF layer before driver code can reclaim request ownership.
- Add a controller-bundle boundary that materializes independent logical
  devices with controller-global queue IDs, plus an explicit single-device
  compatibility adapter for legacy interfaces.
- Add an object-safe discovery-to-ready controller endpoint whose IRQ sources
  are bound before the first bounded initialization command is submitted.
- Split every logical IRQ source into a bounded hard-IRQ capture endpoint and
  an owner-side rearm control. Captured masked sources carry a nonzero
  generation and bitmap so stale work cannot reopen a recovered device epoch.
- Require explicit fail-closed containment when captured facts cannot be
  published. The endpoint receives a typed publication/ownership cause and
  returns the exact masked-source token; an uncontained failure must instead
  close the OS action or parent interrupt line.
- Replace submit/poll queues with owned requests, explicit inline/interrupt
  completion kinds, runtime-assigned request IDs, and IRQ-event service batches.
- Replace permissive direct dispatch metadata with `QueueExecution`:
  `Inline`, owner-side multi-tag `Tagged`, or owner-side `Serialized`.
- Return complete request ownership on submit failure, terminal completion, and
  explicit queue shutdown.
- Require interrupt controllers to expose a nonblocking recovery lifecycle,
  stable identity, and typed DMA-quiesced/controller-ready proofs.
- Require every interface to declare its initialization endpoint explicitly;
  hardware readiness can no longer be inherited from a default implementation.
- Retain an endpoint fail-closed when it is dropped without a successful
  ownership-returning shutdown, preventing live DMA state from being freed.
- Distinguish cancellation-triggered recovery from timeout and queue faults so
  runtimes can preserve the winning request generation through DMA quiescence.
- Return rejected owned requests directly from `SubmitError` without a
  per-rejection heap allocation.
- Cache every queue's static identity, geometry, limits, and interrupt-source
  contract in `QueueHandle`, then close admission at the first invalid static
  contract or submit ownership transition instead of repeatedly entering an
  untrusted endpoint.
- Reserve `RequestId::INLINE` outside the generation/tag space and enforce the
  identity mode before entering a queue, so inline devices require no request
  allocator while interrupt queues cannot alias the sentinel.
- Make queue shutdown a one-shot `Live → Attempted → Closed` transaction;
  failed driver teardown now retains the endpoint in quarantine and all later
  operations fail offline without re-entering driver code.
- Return an explicit quarantine owner when unpublished-device rollback cannot
  close every queue, retaining both the original contract failure and the
  close diagnostics instead of dropping them with a temporary collection.

### Fixed

- Validate request byte lengths with checked `usize` arithmetic instead of
  truncating large logical block sizes through `u32`.
- Validate DMA direction, translation domain, address mask, and alignment at
  the owned-request boundary, and reject writes to read-only devices.
- Validate owned DMA length against the complete scatter/gather segment budget
  instead of one segment's limit.
- Keep captured IRQ facts, device-source masking ownership, and the source
  generation in one typed result so lifecycle routing cannot lose rearm state.
- Make initialization-schedule wake conditions private, add an explicit
  validation boundary and read-only accessors, and provide a constructor for
  the common IRQ-or-absolute-deadline wait.
- Require metadata-only flush requests to use the canonical zero LBA.
- Reject queue identity conflicts and static interrupt-queue contract failures
  before a logical device can be published.
- Reject zero-capacity device geometry and unusable DMA/request limits before
  queue activation.
- Reject task-side event batches routed to a different hardware queue.
- Report the expected interrupt and actual inline lifecycles in the correct
  order when an inline queue is passed to controller binding.
- Add a canonical submit-result validator for queue-kind and generation-bearing
  request-ID ownership transitions.
- Document compile-time non-forgeability and linear consumption of controller
  DMA-quiescence and ready proofs.

### Removed

- Remove borrowed request segments, polling APIs, and the `POLLED` request flag.

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

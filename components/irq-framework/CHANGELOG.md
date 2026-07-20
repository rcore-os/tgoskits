# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Add move-only detached IRQ action ownership so passthrough runtimes can
  remove a disabled host action, install an exclusive guest route, and later
  re-register the original handler without retaining a dormant descriptor
  entry.
- Split hard-IRQ isolation into `DisableActionAndWake`, for sources already
  masked precisely at the device, and fail-closed `MaskLineAndWake`, which
  quenches the affected controller line without disabling the action. Shared
  lines reopen only after every action-owned quench is explicitly released and
  the matching controller claim has completed.
- Treat failure to apply that emergency backing-line mask as a fatal irqchip
  invariant instead of silently returning to a potentially storming source.
- Add generation-bearing, action-specific asynchronous drain tokens with a
  fixed hard-IRQ-safe wake target.

### Changed

- Remove the generic deferred-continuation token/slot protocol. Controller claim
  completion now belongs to the dispatch RAII boundary, and task-side recovery
  chooses explicit action isolation or fail-closed line masking.
- Make new IRQ requests disabled by default, requiring callers to publish all
  device-side handler state before an explicit `Registry::enable` operation.
- Commit a successful registration to the descriptor's desired line state, so
  a disabled first owner stays masked while enabled shared peers are restored.
- Keep newly requested actions disabled until affinity and controller-line
  setup commit, so failed registration cannot expose a partially published
  handler to a late IRQ.
- Require per-CPU quench recovery to name the affected CPU explicitly; one
  CPU's recovery no longer clears another CPU's independent quench ownership.

### Fixed

- Reject registration and CPU-online bookkeeping from hard-IRQ context before
  either path can enter controller-facing or allocation-backed work.
- Treat `IrqExecution` as an action-local contract, allowing concurrent and
  non-reentrant handlers to share one compatible hardware line, and let an
  unconstrained `Any` action inherit that line's existing fixed affinity.
- Prevent action IDs and drain generations from wrapping into stale handles or
  tokens, and make descriptor reader-count overflow/underflow fatal before it
  can permit premature reclamation.
- Invoke drain wake callbacks outside registry metadata critical sections and
  pin the descriptor until immediate notification returns.
- Preserve a racing `MaskLineAndWake` action during `free` and failed-request
  rollback until its in-flight invocation has drained.

## [0.3.0](https://github.com/rcore-os/tgoskits/compare/irq-framework-v0.2.0...irq-framework-v0.3.0) - 2026-07-02

### Added

- *(irq-framework)* use domain-scoped irq ids

### Other

- *(irq-framework)* require boxed IRQ callbacks ([#1452](https://github.com/rcore-os/tgoskits/pull/1452))
- *(somehal)* modernize x86 qemu irq routing ([#1430](https://github.com/rcore-os/tgoskits/pull/1430))

## [0.2.0](https://github.com/rcore-os/tgoskits/compare/irq-framework-v0.1.1...irq-framework-v0.2.0) - 2026-06-27

### Other

- *(serial)* align IRQ model with dev ([#1265](https://github.com/rcore-os/tgoskits/pull/1265))

## [0.1.1](https://github.com/rcore-os/tgoskits/compare/irq-framework-v0.1.0...irq-framework-v0.1.1) - 2026-06-12

### Added

- *(irq)* enhance IRQ request handling and state restoration logic

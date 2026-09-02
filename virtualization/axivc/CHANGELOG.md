# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Added allocation-free Message V1 framing with nonblocking fragmented send,
  receive, discard, and abort state machines.
- Added explicit protocol errors for malformed frames, inconsistent message
  metadata, output-buffer exhaustion, and transfer aborts.
- Added streaming, malformed-input, long-message, SPSC concurrency, and
  ArceOS full-duplex coverage.

### Changed

- Changed ring storage into opaque 64-byte cells and moved Request/Ack plus
  application sequence semantics out of `axivc` payload transport.
- Upgraded the shared region layout from v2 to v3. This is intentionally
  incompatible with v2 peers; the publish/subscribe/notify HVC ABI is unchanged.
- Migrated the ArceOS publisher and subscriber demos to application-owned
  Request/Ack/Data payloads with strict sequence, length, and body validation
  across fragment, ring-capacity, and backpressure boundaries.

### Removed

- Removed the fixed 48-byte `IvcProducer`/`IvcConsumer` API and
  `IvcMessageKind` application protocol from the transport crate.

### Compatibility

- The external Linux `axvisor.ko` companion has not yet been migrated to region
  v3 and must be updated before the ArceOS-to-Linux QEMU case is compatible.

## [0.1.0] - 2026-07-15

### Added

- Initial `axivc` crate for AxVisor inter-VM shared-memory communication.
- Added fixed shared-memory region layout and two SPSC message rings.
- Added request and acknowledgement message helpers.
- Added peer-event wait helpers for IRQ wakeup with bounded fallback polling.
- Added English and Chinese README files, Apache-2.0 license text, and crate
  local ignore rules.

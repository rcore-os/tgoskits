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

- Changed ring storage into opaque 256-byte slots and moved Request/Ack plus
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

- 共享内存布局采用 256 字节 slot，每个 ring 包含 32 个槽位，region 总大小为
  17152 字节；两个 ring 的偏移为 256、8704，各占 8448 字节。region 版本仍为 3，
  旧版 v3 通过布局校验被拒绝；通信双方在升级或回滚时必须保持布局一致。
- 将 `cell` 类型、常量、方法和 `CellFull` 错误统一改名为 `slot`/`SlotFull`，
  同步两个 ArceOS demo 的分片边界和满 ring 消息长度，不保留旧接口别名。

## [0.1.1](https://github.com/rcore-os/tgoskits/compare/axivc-v0.1.0...axivc-v0.1.1) - 2026-09-09

### Added

- feat(StarryOS)：Enhance axivc IVC char devices and improve ioctl handling ([#2214](https://github.com/rcore-os/tgoskits/pull/2214))

### Fixed

- *(repo)* remove redundant Cargo manifest declarations ([#2297](https://github.com/rcore-os/tgoskits/pull/2297))

## [0.1.0] - 2026-07-15

### Added

- Initial `axivc` crate for AxVisor inter-VM shared-memory communication.
- Added fixed shared-memory region layout and two SPSC message rings.
- Added request and acknowledgement message helpers.
- Added peer-event wait helpers for IRQ wakeup with bounded fallback polling.
- Added English and Chinese README files, Apache-2.0 license text, and crate
  local ignore rules.

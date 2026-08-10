<h1 align="center">axivc</h1>

<p align="center">Shared-memory protocol helpers for AxVisor inter-VM communication</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/axivc.svg)](https://crates.io/crates/axivc)
[![Docs.rs](https://docs.rs/axivc/badge.svg)](https://docs.rs/axivc)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Overview

`axivc` provides reusable `no_std` shared-memory protocol helpers for AxVisor
inter-VM communication. It is used after AxVisor has mapped the same IVC channel
into two guests.

The crate owns the guest-visible protocol layout and in-memory operations:

- a fixed shared-memory region header;
- two single-producer/single-consumer message rings;
- fixed-size request and acknowledgement messages;
- peer-event counters for IRQ wakeup plus bounded fallback polling.

`axivc` intentionally does not issue hypercalls, register IRQ handlers, or map
guest physical addresses. Those operations belong to lower-level ABI crates and
guest OS integration code.

# Layering

The IVC stack is split into three layers:

- `axhvc`: raw guest-hypervisor ABI, including hypercall numbers, register
  argument order, architecture-specific trap instructions, and low-level
  publish, subscribe, notify, and unpublish wrappers.
- `axivc`: architecture-independent shared-memory protocol layout and
  operations after a channel has been mapped.
- guest OS glue: virtual-to-physical translation for hypercall output slots,
  GPA mapping, IRQ registration, scheduler wakeup, and application policy.

A complete guest flow usually calls `axhvc` to publish or subscribe to a channel,
maps the returned GPA through the guest OS, and then treats the mapped memory as
an `axivc::IvcRegion`.

# Protocol

The current protocol is a compact single-page format:

- The first two `u64` fields match AxVisor's host-side `IVCChannelHeader`
  layout: publisher VM ID and channel key.
- `IvcRegion` records magic, version, region size, feature flags, and ring
  offsets.
- Two fixed-slot rings are provided: publisher-to-subscriber and
  subscriber-to-publisher.
- Each slot carries message kind, sequence number, payload length, and a fixed
  payload buffer.

The ring protocol uses Release/Acquire ordering. The producer writes a slot
payload and releases `tail`; the consumer acquires `tail`, copies the slot, and
releases `head` to return ownership.

# Memory Attributes

The current AxVisor IVC protocol assumes that the shared-memory window is mapped
as Normal cacheable memory by every guest and that the target platform provides
cache coherency between those guests. On aarch64 QEMU this matches the current
Zephyr/Linux test setup:

- Zephyr maps the channel with `K_MEM_CACHE_WB`.
- Linux maps the reserved-memory region through the IVC driver mmap path.
- AxVisor marks the generated `ivc-channel` FDT node as `dma-coherent`.

Release/Acquire ordering only orders CPU accesses. It does not clean or
invalidate private caches on non-cache-coherent platforms. Such platforms need
an OS-specific cache maintenance layer around the same protocol publish/observe
points before this crate can be used safely there.

# Guest Flow

A publisher typically:

1. Calls `axhvc::ivc::publish_channel`.
2. Maps the returned shared-memory GPA.
3. Initializes the mapped memory with `IvcRegion::initialize`.
4. Attaches `IvcRegion::publisher_endpoints` exactly once and splits the result
   with `IvcEndpoints::into_parts`.
5. Sends through the producer and receives through the consumer.
6. Optionally notifies the peer through `axhvc`.

A subscriber typically:

1. Calls `axhvc::ivc::subscribe_channel`.
2. Maps the returned shared-memory GPA.
3. Validates `channel_header_matches` and `protocol_header_matches`.
4. Attaches `IvcRegion::subscriber_endpoints` exactly once and splits the
   result with `IvcEndpoints::into_parts`.
5. Receives through the consumer and replies through the producer.
6. Optionally notifies the peer through `axhvc`.

For blocking-style receive paths, guest IRQ code can call `record_peer_event`
when AxVisor injects a notify IRQ. The receive path can then use
`IvcPeerEventWaiter` and `fallback_poll` to combine IRQ wakeup with bounded
polling when an interrupt is missed or not yet wired.

# Current Limits

- The region layout fits in one 4 KiB page; AxVisor IVC channels may be larger
  (up to the hypervisor's `MAX_IVC_CHANNEL_SIZE`), and the extra space is
  currently unused by this protocol.
- Each HVC channel currently admits one publisher and one subscriber because
  both rings are single-producer/single-consumer. Multi-peer support is tracked
  in [tgoskits#1238](https://github.com/rcore-os/tgoskits/issues/1238) and will
  require a versioned per-peer memory layout.
- Payload slots are fixed size.
- OS IRQ registration and hypervisor notification hypercalls are outside this
  crate.
- Access control, quotas, and channel lifecycle remain AxVisor or guest policy.

# Development

Use the workspace `xtask` flow for validation:

```bash
cargo fmt
cargo xtask clippy --package axivc
```

# License

This project is licensed under the Apache License 2.0. See [LICENSE](./LICENSE)
for details.

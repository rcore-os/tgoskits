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

# Guest Flow

A publisher typically:

1. Calls `axhvc::ivc::publish_channel`.
2. Maps the returned shared-memory GPA.
3. Initializes the mapped memory with `IvcRegion::initialize`.
4. Sends requests with `IvcRegion::send_request`.
5. Optionally receives acknowledgements with `IvcRegion::try_recv_ack`.
6. Optionally notifies the peer through `axhvc`.

A subscriber typically:

1. Calls `axhvc::ivc::subscribe_channel`.
2. Maps the returned shared-memory GPA.
3. Validates `channel_header_matches` and `protocol_header_matches`.
4. Receives requests with `IvcRegion::try_recv_request`.
5. Optionally replies with `IvcRegion::send_ack`.
6. Optionally notifies the peer through `axhvc`.

For blocking-style receive paths, guest IRQ code can call `record_peer_event`
when AxVisor injects a notify IRQ. The receive path can then use
`IvcPeerEventWaiter` and `fallback_poll` to combine IRQ wakeup with bounded
polling when an interrupt is missed or not yet wired.

# Current Limits

- The region is designed to fit in the current 4 KiB AxVisor IVC channel.
- Rings are single-producer/single-consumer.
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

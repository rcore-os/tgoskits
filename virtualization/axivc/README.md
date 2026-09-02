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

`axivc` is a `#![no_std]` shared-memory protocol crate used after AxVisor maps
one IVC channel into a publisher and a subscriber. It provides:

- two independent single-producer/single-consumer directions;
- fixed-size opaque cell rings with Release/Acquire publication;
- Message V1 framing, validation, fragmentation, reassembly, and aborts;
- nonblocking partial-progress APIs for messages larger than the whole ring;
- peer-event helpers for IRQ wakeup with bounded fallback polling.

Hypercalls, GPA mapping, IRQ registration, blocking waits, notifications, and
application protocols remain in guest OS glue. In particular, request/ack kinds
and application sequence numbers are payload bytes; they are not transport
fields.

# Layering

```text
application payload (RPC, request/ack, file chunk, ...)
IvcMessageSender / IvcMessageReceiver (Message V1 cells)
opaque SPSC rings in IvcRegion
AxVisor HVC mapping and guest notify glue
```

The ring never interprets a cell. Message framing never invokes a hypercall or
runtime service.

# Message V1

Each 64-byte cell starts with a manually encoded 24-byte little-endian header:

| Offset | Size | Field |
|---:|---:|---|
| `0x00` | 1 | version (`1`) |
| `0x01` | 1 | `FIRST`, `LAST`, `ABORT` flags |
| `0x02` | 2 | header length (`24`) |
| `0x04` | 4 | fragment length |
| `0x08` | 8 | transport message ID |
| `0x10` | 8 | complete payload length |
| `0x18` | up to 40 | fragment bytes |

Frames of one message are contiguous. The sender assigns nonzero transport IDs,
automatically writes flags and lengths, and never interleaves messages. The
receiver rejects unknown versions/flags, malformed lengths, changed IDs or total
lengths, and incorrect `LAST` boundaries.

The shared region layout version is **3**, incompatible with the old v2
fixed-request/ack cell format. A v2 peer and a v3 peer explicitly reject each
other. The publish/subscribe/notify HVC ABI is unchanged.

# Nonblocking API

Start a message, then repeatedly provide the unconsumed input suffix:

```rust
# use axivc::{IvcMessageError, IvcMessageSender};
fn send_step(
    sender: &mut IvcMessageSender<'_>,
    payload: &[u8],
    consumed: &mut usize,
) -> Result<bool, IvcMessageError> {
    let progress = sender.try_write(&payload[*consumed..])?;
    *consumed += progress.consumed();
    // Guest glue may notify once when published_cells() is nonzero.
    Ok(progress.is_complete())
}
```

`try_write` returns zero progress when the ring is full and preserves sender
state for a later retry. A message can therefore exceed both one cell and all
16 in-flight cells.

A receiver may inspect untrusted metadata without consuming `FIRST`:

```rust
# use axivc::{IvcMessageError, IvcMessageReceiver};
fn receive_step(
    receiver: &mut IvcMessageReceiver<'_>,
    output: &mut [u8],
) -> Result<bool, IvcMessageError> {
    let progress = receiver.try_read(output)?;
    // Append only output[..progress.written()] to the application sink.
    Ok(progress.is_complete())
}
```

A cell fragment is never partially consumed. If the first available fragment
does not fit, `BufferTooSmall` is returned and the ring head is unchanged. Use
`try_discard` to drain a message rejected by application resource policy.
Callers that need application-level atomic visibility must stage streaming
output themselves until `LAST` is observed.

# Guest Flow

1. Publish or subscribe with `axhvc` and map the returned GPA.
2. The publisher calls `IvcRegion::initialize`.
3. The subscriber validates `channel_header_matches` and
   `protocol_header_matches`.
4. Attach exactly once with `publisher_endpoints` or `subscriber_endpoints`.
5. Split with `IvcEndpoints::into_parts` and move sender/receiver into their
   owning tasks.
6. Notify the peer after publishing cells or releasing ring capacity; wait or
   poll when an operation makes no progress.

The unsafe attachment contract is what prevents duplicate producers or
consumers from racing on `UnsafeCell` cell bytes.

# Compatibility and Limits

- Each HVC channel currently admits one publisher and one subscriber because
  both rings are single-producer/single-consumer. Multi-peer support is tracked
  in [tgoskits#1238](https://github.com/rcore-os/tgoskits/issues/1238) and will
  require a versioned per-peer memory layout.
- Cell size is 64 bytes, fragment capacity is 40 bytes, and ring capacity is 16.
- Message V1 does not interleave messages, retransmit, reorder, or allocate.
- Peer-reset reporting is reserved in the API; the current HVC backend has no
  queue generation and cannot produce it yet.
- The external Linux `/root/axvisor.ko` companion must be upgraded to region v3
  before ArceOS-to-Linux IVC is compatible. This repository does not contain
  that module.
- ivshmem, PCI BARs, doorbells, MSI-X, and owner-RW/peer-RO sections are outside
  this protocol revision.

# Development

Use the workspace `xtask` flow for validation:

```bash
cargo fmt
cargo test -p axivc
cargo xtask clippy --package axivc
cargo xtask axvisor test qemu --arch aarch64 --test-case ivc-arceos2arceos
```

# License

This project is licensed under the Apache License 2.0. See [LICENSE](./LICENSE)
for details.

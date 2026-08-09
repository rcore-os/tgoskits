# axvirtio-net

A `no_std` VirtIO 1.x MMIO **network device model** for the
[ArceOS-Hypervisor](https://github.com/arceos-hypervisor/) ecosystem. It runs
on the host side of a hypervisor and emulates a virtio-net device that a guest
driver talks to over MMIO; it is **not** a guest-side virtio-net frontend
driver.

It is built on top of [`axvirtio-common`](../axvirtio-common), which owns the
split virtqueue and MMIO transport protocol logic.

## Architecture

```text
VMM / network runtime (TAP, virtual switch, IRQ injection, task scheduling)
  |  push RX frame via receive_frame()   /   consume DeviceEvent to inject IRQ
  v
axvirtio-net  (this crate)
  |  virtio_net_hdr wire format, RX/TX semantics, config space, backend calls
  v
axvirtio-common  (split virtqueue, MMIO transport, guest memory access)
  |  GuestMemoryAccessor
  v
guest address space
```

The portable device model owns only protocol state. TAP/virtual-switch
lifetime and virtual interrupt injection belong to the VMM glue layer, which is
intentionally out of scope.

## First-version scope

- VirtIO 1.x MMIO transport, device ID `1` (network).
- Split virtqueue with a single RX/TX queue pair (RX = queue 0, TX = queue 1).
- Advertised features: `VIRTIO_F_VERSION_1`, `VIRTIO_NET_F_MAC`,
  `VIRTIO_NET_F_STATUS`.
- Base 10-byte `virtio_net_hdr` (no mergeable buffers). RX writes a zero
  header; TX rejects any checksum/GSO offload request.
- 6-byte MAC and link status exposed in config space (byte/word/dword reads).
- Guest TX is drained on queue-1 notification; host RX is driven explicitly by
  the VMM calling `receive_frame`.

Out of scope (do **not** appear in the device feature bits): control queue,
multiqueue (`VIRTIO_NET_F_MQ`), mergeable buffers (`VIRTIO_NET_F_MRG_RXBUF`),
indirect descriptors, `VIRTIO_F_RING_EVENT_IDX`, checksum/GSO/TSO offload, RSS.

## Public API surface

```rust
use axvirtio_net::{
    VirtioMmioNetDevice, VirtioNetConfig, NetworkBackend, NetworkBackendError, DeviceEvent,
    RxOutcome,
};

// Implement the host transmit boundary.
struct MyBackend;
impl NetworkBackend for MyBackend {
    fn transmit(&self, frame: &[u8]) -> Result<(), NetworkBackendError> { /* ... */ Ok(()) }
}

let device = VirtioMmioNetDevice::new(
    mmio_base, mmio_len, MyBackend, VirtioNetConfig::default(), guest_memory,
)?;

// Guest MMIO trap handler:
match device.mmio_write(addr, width, value)? {
    DeviceEvent::InterruptPending => { /* VMM injects a virtual IRQ */ }
    DeviceEvent::Reset => { /* device fully reset */ }
    DeviceEvent::None => {}
}
let v = device.mmio_read(addr, width)?;

// Host -> guest RX (VMM calls this when a frame arrives):
match device.receive_frame(frame)? {
    RxOutcome::Delivered { frame_len } => { /* written into a guest buffer */ }
    RxOutcome::NoGuestBuffer => { /* VMM may cache/retry/drop */ }
}
```

## Concurrency contract

- The device uses short internal critical sections (per-field spin locks, one
  per-queue lock).
- The TX path calls `NetworkBackend::transmit` while holding the queue lock, so
  **backends must not re-enter the device** from within `transmit` (e.g. call
  `receive_frame`), or it will self-deadlock.
- RX (`receive_frame`) validates the whole chain capacity **before** consuming
  the available head, so a too-small buffer or bad chain leaves the ring
  untouched.

## License

Licensed under Apache-2.0 (compatible with this repository).

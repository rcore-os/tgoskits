# AxVisor virtio-net device design

## Problem and success criteria

AxVisor must boot two ArceOS guests, expose one VirtIO MMIO network device to
each guest, and forward Ethernet frames between them. Completion requires both
guests to discover their device and pass a deterministic bidirectional network
test without MMIO faults, queue stalls, or descriptor leaks.

The initial scope is an in-process layer-2 switch. A physical host NIC uplink,
multi-queue VirtIO, offloads, live migration, and passthrough are non-goals.

## Existing code and rejected reuse

`debin/virtio-net-2` contains reusable `axvirtio-common` and `axvirtio-net`
crates plus an older AxVisor adapter. Those network-related crates and their
tests are retained; `axvirtio-blk` is deferred to a separate change with a real
block-device consumer. The adapter is not copied unchanged because current
`axdevice` grants guest-memory DMA only through a scoped `DeviceAccess` port.
The old adapter retained a VM-wide guest-memory accessor in a background
worker, bypassing that capability boundary.

## Selected architecture

The implementation has four explicit layers:

1. `axvirtio-common` owns VirtIO MMIO state and split-ring mechanics. Queue
   layout persists, but every operation that reads or writes guest memory
   receives a scoped memory capability.
2. `axvirtio-net` owns RX/TX descriptor validation and the device state
   machine. It has no AxVisor or ArceOS dependency.
3. `axdevice` adds a DMA-pollable capability registered against a concrete
   bundled device and a `DmaGrant`. The runtime reconstructs a scoped
   `DeviceAccess` for each poll; the capability cannot retain it.
4. AxVisor registers a `virtio-net` `DeviceModel`. Each instance owns one
   switch port, MMIO allocation, wired IRQ, and DMA grant. Device polling drains
   bounded ingress frames into the guest RX ring and pulses the IRQ only after
   publishing used descriptors.

```text
guest TX kick
  -> Device::access(scoped DMA)
  -> validate/read TX chain
  -> switch classifies frame
  -> destination bounded ingress queue

vCPU0 device poll
  -> DMA-poll capability(scoped DMA)
  -> write destination RX chain and used ring
  -> publish interrupt status
  -> pulse destination wired IRQ
```

## Ownership and synchronization

- `DeviceModel` owns immutable per-NIC configuration.
- The built device owns VirtIO state and its DMA grant.
- Each switch attachment has a unique generation-bearing port identity; its
  RAII registration removes the port on teardown.
- TX processing occurs under the device queue lock. The switch backend only
  enqueues copies and never re-enters a VirtIO device.
- RX ingress is bounded. A full port drops only that destination copy.
- Device polling owns RX advancement. IRQ injection happens after guest-memory
  writes complete and outside queue mutation where practical.
- The scoped `DeviceAccess` reference is never stored, shared, or moved into a
  task. This preserves VM lifetime and DMA authorization.

## Failure behavior

Invalid descriptors, inaccessible guest memory, unsupported features, and
resource-planning failures return typed errors. Queue exhaustion and absent RX
buffers are observable flow-control outcomes, not fabricated success. No path
falls back to a guessed MMIO address, IRQ, MAC address, or host interface.

## Alternatives

| Alternative | Decision |
| --- | --- |
| Retain a weak `AxVM` guest-memory accessor in a worker | Rejected: bypasses access-scoped DMA grants and couples the device to AxVM. |
| Deliver RX only on guest MMIO kicks | Rejected: frames arriving after the last RX kick can stall forever. |
| Add a generic DMA-pollable capability | Selected: keeps authorization explicit and supports asynchronous device progress without retaining VM memory. |
| Start with a physical uplink worker | Deferred: unnecessary for proving two-guest VirtIO networking and adds host-driver ownership risk. |

## Validation

- Existing split-ring, MMIO, block, net, and switch tests must continue to pass.
- Add capability tests proving an unregistered/wrong DMA grant is rejected and
  the correct device receives scoped memory during polling.
- Add AxVisor model tests for options, resource requirements, and bundle grants.
- Boot two ArceOS guests under QEMU/AxVisor and require deterministic two-way
  packet exchange. From a clean checkout, build each image before launching
  AxVisor (the VM TOML files intentionally reference these generated files):

  ```bash
  apps/arceos/virtio-net-peer/run.sh
  ```

  Set `LLVM_OBJCOPY` when the toolchain is installed outside the pinned Rust
  sysroot.

  The QEMU runner requires both `VM1_VIRTIO_NET_PASS` and
  `VM2_VIRTIO_NET_PASS`; either `*_FAIL` marker or a panic is a failure.
- Run `cargo fmt` and targeted clippy for every changed crate.

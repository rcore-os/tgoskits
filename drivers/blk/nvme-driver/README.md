# NVMe Driver

Portable NVMe 1.4 block driver for the `rdif-block` capability boundary.

## RDIF Submit/IRQ Model

The RDIF data path is queue-local and non-blocking:

- `submit_owned()` validates the LBA request, allocates a queue-local CID, builds PRP entries, writes one SQE, rings the submission doorbell, and transfers request ownership to that queue.
- The hard-IRQ endpoint acknowledges a globally bounded CQ batch into a preallocated queue-local cache and emits a queue event.
- `service_events()` consumes that IRQ snapshot and may continue draining the same acknowledged CQ in bounded worker batches only when the hard-IRQ endpoint yielded an explicit continuation because its 64-completion budget expired or CQ ownership was contended. A cache-only event cannot read the CQ again. The worker resolves the matching runtime `RequestId`, returns request ownership through `CompletionSink`, and rings the completion doorbell; no timer or request thread probes the CQ.
- Queue-full or CID exhaustion returns the unaccepted request with `BlkError::Retry`; an accepted request reaches exactly one terminal completion through an IRQ event or typed recovery.

Discovery only maps the BAR, validates capabilities, allocates retained DMA storage, and keeps device interrupt sources masked. `InitialController` performs disable/enable, Identify Controller, queue creation, and namespace identification only after the OS has installed its initialization IRQ action. The IRQ endpoint caches each admin completion, and the state machine consumes that cache only when `InitInput` names the admin source. Capacity and normal queues are not published before `Ready`.

Controller recovery uses the same IRQ-cached admin completion boundary through `InterruptLifecycle`. Absolute deadlines detect reset or command failure; they never inspect a completion queue as a fallback. The public block data path has no completion-query or synchronous read/write API and does not spin for hardware completion.

## Queues, PRP, And CID

Each RDIF queue owns one hardware IO queue pair: SQ, CQ, CID slots, PRP list pages, and doorbell access. Request address fields are device-native `lba` and `block_count`; Linux-style 512-byte sector translation belongs to OS glue above `rdif-block`.

Read and write requests use NVMe PRP:

- `prp1` points at the first DMA page fragment.
- `prp2` is either the second page or a PRP-list page.
- The current implementation supports one PRP-list page per request.

Flush, discard, and write-zeroes are reported as unsupported until Identify/feature capability validation is plumbed for those commands.

## IRQ Sources

`rdif-block` supports multiple IRQ sources via `Interface::irq_sources()` and `take_irq_handler(source_id)`. NVMe maps INTx source 0 or each retained MSI-X vector to the queues routed to that vector. Activation fails closed if the admin vector or an I/O queue vector lacks a platform binding or handler.

The IRQ handler performs the first destructive CQ read under a queue-local claim, stores completion metadata in fixed cache slots, and returns a queue event. It does not transfer request ownership, wake arbitrary tasks, allocate, or take OS locks. The shared block worker consumes the cached metadata, performs any bounded CQ continuation authorized by that event, and issues directed completion wakeups.

## QEMU Smoke Test

The StarryOS NVMe rootfs test boots with an NVMe disk and installs curl inside the guest:

```shell
cargo xtask starry test qemu --arch x86_64 -c nvme-rootfs-apk-curl
```

The same case is defined for `aarch64`, `riscv64`, and `loongarch64`.

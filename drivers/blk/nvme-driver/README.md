# NVMe Driver

Portable NVMe 1.4 block driver for the `rdif-block` capability boundary.

## RDIF Submit/IRQ Model

The RDIF data path is queue-local and non-blocking:

- `submit_owned()` validates the LBA request, allocates a queue-local CID, builds PRP entries, writes one SQE, rings the submission doorbell, and transfers request ownership to that queue.
- The hard-IRQ endpoint masks its exact controller vector and emits the immutable queue routing frozen when that action was registered. It cannot access an admin queue, I/O CQ, request slot, or doorbell.
- `service_events()` consumes that IRQ snapshot and may keep draining the same acknowledged CQ in bounded owner-thread batches while the exact device vector remains masked. Only its `QueueEventBatch` can mint a `ServiceRerun`; a cache-only call cannot probe the CQ. The owner resolves the matching runtime `RequestId`, returns request ownership through `CompletionSink`, and rings the completion doorbell; no timer or request thread probes the CQ.
- Queue-full or CID exhaustion returns the unaccepted request with `BlkError::Retry`; an accepted request reaches exactly one terminal completion through an IRQ event or typed recovery.

Discovery only maps the BAR, validates capabilities, allocates retained DMA storage, and keeps device interrupt sources masked. `InitialController` performs disable/enable, Identify Controller, queue creation, and namespace identification only after the OS has installed its initialization IRQ action. The maintenance owner consumes one admin CQ entry only when `InitInput` names the admin source. Capacity and normal queues are not published before `Ready`.

Controller recovery uses the same owner-only, IRQ-evidenced admin completion boundary through `InterruptLifecycle`. Absolute deadlines detect reset or command failure; they never inspect a completion queue as a fallback. The public block data path has no completion-query or synchronous read/write API and does not spin for hardware completion.

## Queues, PRP, And CID

Each RDIF queue owns one hardware IO queue pair: SQ, CQ, CID slots, PRP list pages, and doorbell access. Request address fields are device-native `lba` and `block_count`; Linux-style 512-byte sector translation belongs to OS glue above `rdif-block`.

Read and write requests use NVMe PRP:

- `prp1` points at the first DMA page fragment.
- `prp2` is either the second page or a PRP-list page.
- The current implementation supports one PRP-list page per request.

Flush, discard, and write-zeroes are reported as unsupported until Identify/feature capability validation is plumbed for those commands.

## IRQ Sources

`rdif-block` supports multiple IRQ sources via `Interface::irq_sources()` and `take_irq_source(source_id)`. NVMe maps INTx source 0 or each retained MSI-X vector to the queues routed to that vector. Activation fails closed if the admin vector or an I/O queue vector lacks a live platform binding and capture endpoint.

The IRQ capture endpoint owns only `NvmeIrqState`: INTMS/INTMC access, source generations, mask state, and a frozen queue bitmap. It masks its exact vector and returns that stable bitmap plus a generation token. The CPU-pinned maintenance owner is the only context that reads CQ entries, advances CQ heads, rings SQ/CQ doorbells, or publishes request completion. The separate owner-side control endpoint rearms only the matching masked generation after bounded service completes; scheduling and wake policy remain outside this portable driver.

## QEMU Smoke Test

The StarryOS NVMe rootfs test boots with an NVMe disk and installs curl inside the guest:

```shell
cargo xtask starry test qemu --arch x86_64 -c nvme-rootfs-apk-curl
```

The same case is defined for `aarch64`, `riscv64`, and `loongarch64`.

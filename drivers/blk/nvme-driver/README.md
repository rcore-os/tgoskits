# NVMe Driver

Portable NVMe block driver for the IRQ-driven `rdif-block` capability
boundary. Register and command decisions are based on NVM Express Base
Specification 2.2.

## RDIF Owned/IRQ Model

Each hardware queue is moved into one maintenance task:

- `submit_owned()` validates the LBA and every DMA constraint again, allocates a
  queue-local CID, builds PRP entries, writes one SQE, rings the submission
  doorbell, and transfers DMA ownership to hardware.
- `drain_completions()` runs only after the queue's fixed IRQ endpoint has
  acknowledged an event. It drains CQEs, rings the completion doorbell, and
  returns terminal status plus DMA ownership through the completion sink.
- `RequestId` is the NVMe CID for the same IO queue. It must not be used on another queue.
- Queue-full or CID exhaustion is reported as `BlkError::Retry`.

Controller/admin initialization is an IRQ-driven `BlockController` state
machine. Register-only disable/enable transitions may be retried under the
runtime's unified deadline; Identify, queue creation, and namespace discovery
advance only after the admin IRQ. There are no public synchronous helpers or
completion-polling mode.

## Queues, PRP, And CID

Each RDIF queue owns one hardware IO queue pair: SQ, CQ, CID slots, PRP list pages, and doorbell access. Request address fields are device-native `lba` and `block_count`; Linux-style 512-byte sector translation belongs to OS glue above `rdif-block`.

Read and write requests use NVMe PRP:

- `prp1` points at the first DMA page fragment.
- `prp2` is either the second page or a PRP-list page.
- The current implementation supports one PRP-list page per request.

Flush maps to NVMe NVM Flush. Discard and write-zeroes are reported as unsupported until the command set implementation grows those operations.

## IRQ Sources

MSI-X reserves one vector for the admin queue and assigns one fixed vector to
each I/O queue/hctx. If fewer than two MSI-X vectors are available, probe
unwinds MSI-X and explicitly constructs a single-queue INTx controller.

The boxed hard handler only verifies/acknowledges its source and publishes a
queue/control bit. INTx additionally checks the PCI function's INTx status,
masks NVMe vector zero with `INTMS`, and requests deferred rearm; the
maintenance task drains the CQ and the controller unmasks through `INTMC`.
Handlers never drain queues, allocate, query `rdrive`, or complete callers.

## QEMU Smoke Test

The StarryOS NVMe rootfs test boots with an NVMe disk and installs curl inside the guest:

```shell
cargo xtask starry test qemu --arch x86_64 -c nvme-rootfs-apk-curl
```

The same case is defined for `aarch64`, `riscv64`, and `loongarch64`.

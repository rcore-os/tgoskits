# NVMe Driver

Portable NVMe 1.4 block driver for the `rdif-block` capability boundary.

## RDIF Submit/Poll Model

The RDIF data path is queue-local and non-blocking:

- `submit_request()` validates the LBA request, allocates a queue-local CID, builds PRP entries, writes one SQE, rings the submission doorbell, and returns `RequestId`.
- `poll_request()` drains CQEs without spinning, updates the matching CID slot, rings the completion doorbell, and reports `Pending` or `Complete`.
- `RequestId` is the NVMe CID for the same IO queue. It must not be used on another queue.
- Queue-full or CID exhaustion is reported as `BlkError::Retry`; incomplete commands are reported as `RequestStatus::Pending`.

Controller/admin initialization still uses the driver's internal admin queue flow. The public block data path does not call synchronous read/write helpers and does not spin for IO completion inside `submit_request()`.

## Queues, PRP, And CID

Each RDIF queue owns one hardware IO queue pair: SQ, CQ, CID slots, PRP list pages, and doorbell access. Request address fields are device-native `lba` and `block_count`; Linux-style 512-byte sector translation belongs to OS glue above `rdif-block`.

Read and write requests use NVMe PRP:

- `prp1` points at the first DMA page fragment.
- `prp2` is either the second page or a PRP-list page.
- The current implementation supports one PRP-list page per request.

Flush maps to NVMe NVM Flush. Discard and write-zeroes are reported as unsupported until the command set implementation grows those operations.

## IRQ Sources

`rdif-block` supports multiple IRQ sources via `Interface::irq_sources()` and `take_irq_handler(source_id)`. The current NVMe block adapter intentionally exposes no IRQ source: IO completion queues are created with interrupts disabled and runtime/OS glue advances requests through submit/poll. This avoids pretending that a controller MSI-X completion path is a legacy INTx source.

Future MSI-X support can expose one RDIF IRQ source per completion vector. The IRQ handler should only return queue events; it should not complete requests, wake tasks, or take OS locks. Runtime/OS glue polls the indicated queues after receiving an event.

## QEMU Smoke Test

The StarryOS NVMe rootfs test boots with an NVMe disk and installs curl inside the guest:

```shell
cargo xtask starry test qemu --arch x86_64 -c nvme-rootfs-apk-curl
```

The same case is defined for `aarch64`, `riscv64`, and `loongarch64`.

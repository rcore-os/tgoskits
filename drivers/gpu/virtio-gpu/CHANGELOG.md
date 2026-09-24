# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Boundary kicks are no longer gated on the ring's `should_notify()`, whose
  RING_EVENT_IDX implementation in virtio-drivers 0.13.0
  (`avail_idx >= avail_event + 1` with a plain u16 `>=`) is not wrap-aware.
  A command batch published across the 65536 avail-index wrap suppressed
  every later kick while the host was behind, so the host was never told
  about the batch: QMP forensics during reproducible hangs showed an idle
  host main loop with `last-avail-idx` frozen at 65534 and `inuse: 0` while
  the guest queue exhausted (ring 64 + parking FIFO 128 all live). Every
  boundary notify is now an unconditional MMIO write — always safe per the
  virtio spec, one write per transaction.

- The blocking waits (`wait_fence`, `wait_idle` and the teardown drains built
  on it) are bounded by a 5 s monotonic-clock timeout instead of spinning
  forever. They run under the consumer's global display lock with interrupts
  disabled, so a host that never completed the waited-for work wedged the
  whole guest with no recovery path — the same hazard `enqueue` already avoids
  with its bounded `Error::QueueBusy`. The waits now return the new
  `Error::TimedOut` (surfaces to the DRM consumer as `-ETIMEDOUT`); the queue
  itself stays usable once the host resumes. `VirtIoGpu::new` takes a
  monotonic nanosecond clock for this. Regression test:
  `wait_fence_times_out_when_the_host_never_completes`.
- `pump_completions` no longer wedges permanently (or panics on the pending
  table index) when the device reports a used entry that does not belong to
  any submission on the queue. The used ring is FIFO and a foreign entry can
  never be consumed, so the queue now logs the condition once, marks itself
  broken, and rejects all further operations with the new `Error::QueueBroken`
  instead of stalling silently at the unusable entry. Regression tests:
  `foreign_used_entry_outside_the_table_breaks_the_queue`,
  `foreign_used_entry_inside_the_table_breaks_the_queue`,
  `pump_leaves_the_blocking_requests_entry_for_its_waiter`.
- Removed the concurrent-device variant of the parking-FIFO test: its fake
  device raced the driver on recycled descriptor state and aborted the suite
  nondeterministically (observed on an unmodified tree). Its coverage —
  saturate ring + FIFO, bounded failure, ordered recovery — is already
  deterministic in `enqueue_returns_busy_when_every_slot_is_live_and_host_is_stalled`.

- `enqueue` no longer busy-spins when the ring and the parking FIFO are both
  full and the host is not completing anything. The unbounded spin ran under
  the consumer's global display lock with interrupts disabled, so a host-side
  completion stall of about a second (the parking FIFO fills in one second at
  120 commands/s) wedged a single-vCPU guest whole — silent, unrecoverable, no
  panic. The queue now makes one reclaim pass and returns the new
  `Error::QueueBusy`, which surfaces to the DRM consumer as `-EAGAIN`.
  Documented deviation from Linux: `virtio_gpu_queue_ctrl_sgs` sleeps on
  `ctrlq.ackq` for free vbufs at the same boundary; sleeping requires moving
  the sync-query waits out of the caller's frame, which is the remaining
  alignment work. The sync-path (`request_sync`) wait is unchanged for now —
  its buffers live in the caller's frame, so it cannot report exhaustion
  without an ownership redesign. Regression test:
  `enqueue_returns_busy_when_every_slot_is_live_and_host_is_stalled`.
- Blocking control commands copy their request header into an allocator-owned
  send buffer before submission. Handing the device a borrowed caller buffer is
  unsound on kernels whose `Hal::share` translates addresses with the
  linear-offset formula: a request living on a vmap'd task kernel stack then
  reaches the device through a mistranslated address, and the host rejects the
  garbled command with `ERR_UNSPEC`. Allocator-owned buffers always translate
  faithfully. (Fire-and-forget commands were never affected — the control queue
  copies them into its own heap arenas.)

### Added

- **Async-only control submission** with the Linux virtio_gpu kick model
  (`drivers/gpu/drm/virtio/virtgpu_vq.c`, `virtio_gpu_notify`): every command
  enqueues and returns immediately without kicking — there are no blocking
  variants. The consumer's transaction boundary delivers the batch with at
  most one kick through `ctrl_notify` (pending counter +
  `virtqueue_kick_prepare` gate, like Linux's DRM ioctl-end notify). Device
  error responses are logged by the completion pump instead of being
  returned. Strict virtqueue ordering keeps
  create → attach → transfer → flush → scanout sequences fence-free.
  Commands whose semantics need the host's answer or an observed completion
  wait explicitly: `transfer_from_host_3d` and `resource_unref` drain the
  ring before returning, and the capset/display-info queries keep their
  request/response shape (all internal; the public surface is one
  submission model).
- Fence and completion API on `VirtIoGpu`: `submit_3d_async(ctx, fence_id, cmd)`
  records the caller's fence id (`VIRTIO_GPU_FLAG_FENCE`), `wait_fence(fence_id)`
  blocks until the entry (and everything before it) is popped,
  `fence_completed(fence_id)` is the non-blocking probe, `pump_completions()`
  recycles finished entries and advances the monotonic fence high-water mark
  (call it from the IRQ handler or the polling loop), and `ctrl_notify()`
  delivers a transaction's batch with a single kick (Linux
  `virtio_gpu_notify()`).
- Blocking `submit_3d` records its own fence response as observed when popped,
  so a blocking submit followed by a poll-based `wait_fence` cannot deadlock on
  an already-fired fence.
- State-machine unit tests over a fake transport/HAL covering threshold
  delivery, batched kicks, zero-copy sync round-trips, FIFO parking when the
  ring saturates, bounded degradation past the parking cap, and fence
  high-water ordering.

### Fixed

- Track `VIRTIO_GPU_F_CONTEXT_INIT` as its own negotiated capability and expose
  it through `VirtIoGpu::has_context_init()`. `ctx_create` now returns
  `Unsupported` for a non-zero `context_init` when the feature was not
  negotiated, while the legacy `context_init == 0` VIRGL path keeps working.
- Handle device-configuration interrupts the way Linux
  `virtio_gpu_config_changed_work_func()` does: read `Config.events_read`, set
  `display_changed` and clear the pending bit through `Config.events_clear`. A
  failed config read conservatively reports `display_changed`, and a failed
  clear is ignored, so the interrupt stays handled instead of being dropped.
  `IrqEvent` gained the `display_changed` field; `is_empty` still depends only on
  `queue` and `configuration`.
- Reject an empty `RESOURCE_ATTACH_BACKING` range and any `paddr + length` that
  would wrap the 64-bit address space, for both `resource_attach_backing` and
  each blob guest backing entry.
- Validate the control-response length before parsing. A response larger than
  the receive buffer returns `ResponseTooLarge`, one too short for its type
  returns `InvalidResponse`, and only the bytes the device wrote are parsed, so
  stale receive-buffer contents never leak into a response or a `GET_CAPSET`
  blob. Plain and data-carrying requests share the same validation.
- Allocate the framebuffer DMA before creating the host resource, and publish the
  framebuffer only after the resource, backing attach and scanout all succeed.
  Failures now roll back with `RESOURCE_UNREF` (detaching the backing before the
  DMA is released), and the DMA stays owned when the device cannot be proven to
  have detached.
- Reset the device (`set_status(DeviceStatus::empty())`) in `Drop` before the
  framebuffer DMA is released, so the device stops scanning out of memory that is
  about to be freed.
- Validate blob resource parameters in the core: the blob memory type, `size`
  and flags, per-range lengths and the checked total guest backing. Cross-device
  blobs return `Unsupported` because `VIRTIO_GPU_F_RESOURCE_UUID` is not
  negotiated.

### Changed

- The framebuffer DMA reports its exact requested length instead of the
  page-aligned allocation size, so the page-alignment padding is no longer
  exposed as framebuffer memory.

## [0.1.0] - 2026-09-21

### Added

- Extract the VirtIO GPU protocol core out of the fully vendored `virtio-drivers`
  checkout: 2D display/framebuffer commands plus the virgl 3D path
  (capset/context/resource3d/transfer3d/submit3d/blob), a crate-owned domain
  error, a private DMA RAII wrapper and a transport-independent IRQ event.
- Reuse the published `virtio-drivers = 0.13.0` crate for `Hal`, `Transport`,
  `VirtQueue`, the config-space macros and the base error instead of vendoring it.

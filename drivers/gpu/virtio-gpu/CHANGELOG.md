# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Linux-style asynchronous control-queue submission (new `ctrl` module,
  `ControlQueue`): fire-and-forget commands (`resource_create_2d`,
  `set_scanout`, `set_scanout_blob`, `resource_flush`, `transfer_to_host_2d`,
  context and 3D resource lifecycle, `transfer_to_host_3d`,
  `resource_create_blob`) enqueue into preallocated vbuf slots and return
  immediately; delivery happens with one MMIO write at the transaction
  boundary (`VirtIoGpu::ctrl_notify`, Linux `virtio_gpu_notify()`). Ring-full
  commands park in a bounded FIFO; exhaustion of ring and FIFO reports the
  retryable `Error::QueueBusy` instead of blocking (a stalled host can no
  longer wedge the guest under the consumer's lock).
- Real fences: `VirtIoGpu::submit_3d` allocates a monotonic fence id, sends it
  with `VIRTIO_GPU_FLAG_FENCE` and returns without waiting. Completion is
  observable through `wait_fence` (bounded by a 5 s injected monotonic clock,
  `Error::TimedOut` on a stalled host), `fence_completed` and the
  `pump_completions` service path (the counterpart of Linux's IRQ-driven
  `virtio_gpu_dequeue_ctrl_func`). Teardown commands that return
  device-accessible memory (`resource_unref`, `transfer_from_host_3d`) deliver
  and drain before returning, so their success is the completion proof.
- `VirtIoGpu::new` takes a `clock: fn() -> u64` used by the bounded waits.
- `VirglOps` (rdif-gpu) gained `wait_fence`, `fence_completed` and
  `ctrl_notify`; `submit` returns `Completion::Pending(fence)` and
  `GpuError::TimedOut` joins the error domain. `VirtIoGpuDevice::service_pending`
  delivers and pumps the queue in task context, per the ack-only IRQ-endpoint
  contract.

### Changed

- Device reset (`VirtIoGpu::reset`, also run from `Drop` and every ambiguous
  completion) now invalidates the control queue synchronously: in-flight
  bookkeeping is dropped and every later enqueue, notify, pump or wait fails
  fast with `Error::QueueBroken` (surfaced as `GpuError::DeviceLost`) instead
  of touching the unregistered virtqueue.
- Teardown drains that cannot be confirmed (timeout, broken queue) reset the
  device before the caller releases backing, so an ambiguous teardown can no
  longer race host DMA into freed memory.
- The synchronous commands (`GET_DISPLAY_INFO`, `GET_CAPSET_INFO`,
  `GET_CAPSET`) keep their fence-validated request/response exchange; a reply
  whose fence echo does not match its request resets the device
  (`Error::DeviceLost`) exactly as before.

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

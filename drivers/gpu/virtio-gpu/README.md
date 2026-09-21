# virtio-gpu

Focused, `no_std` VirtIO GPU protocol core for TGOSKits.

This crate carries only the virtio-gpu control protocol that the current display stack drives: the
2D display/framebuffer path and the virgl 3D path (capset, context, `RESOURCE_CREATE_3D`,
`TRANSFER_*_HOST_3D`, `SUBMIT_3D` and `RESOURCE_CREATE_BLOB`). Upstream `virtio-drivers` ships a
fuller GPU driver; EDID, the cursor queue and the multi-scanout helpers are intentionally left out
because nothing in this workspace calls them and the display stack above this crate owns those
semantics.

Transport access, virtqueue handling and DMA mapping are not reimplemented here. The device is built
on the published `virtio-drivers = 0.13.0` surface (`Hal`, `Transport`, `VirtQueue`, the config-space
macros and `Error`); this crate adds the domain types, the wire encoding and the response
validation. There is no dependency on `rdrive`, `rdif-display`, StarryOS, ArceOS or the Linux DRM
UAPI — mapping those onto the types here is the adapter's job.

Response handling is strict. Every control response first has its reported length checked against the
receive buffer: an oversized response returns `ResponseTooLarge`, a response too short for its type
returns `InvalidResponse`, and only the bytes the device actually wrote are parsed, so stale
receive-buffer contents never leak into a response or into a `GET_CAPSET` blob.

Blob resources are validated against the negotiated feature set before any command is sent. A plain
`GUEST` blob needs only `VIRTIO_GPU_F_RESOURCE_BLOB`; `HOST3D` and `HOST3D_GUEST` also need VIRGL.
Guest-backed blobs must supply non-empty backing whose lengths are each non-zero and sum to at least
the blob size, `HOST3D` blobs must carry no guest backing, and `USE_CROSS_DEVICE` is rejected with
`Unsupported` because this crate neither negotiates `VIRTIO_GPU_F_RESOURCE_UUID` nor implements
`RESOURCE_ASSIGN_UUID`.

Negotiated capabilities are tracked separately, not inferred from one another. `has_virgl`,
`has_resource_blob` and `has_context_init` each report the actual negotiation result, and `ctx_create`
rejects a non-zero `context_init` with `Unsupported` unless `VIRTIO_GPU_F_CONTEXT_INIT` was
negotiated, leaving the legacy `context_init == 0` VIRGL path intact.

Device-configuration interrupts are decoded in the core: `ack_interrupt` reads `Config.events_read`,
reports a display change only when `VIRTIO_GPU_EVENT_DISPLAY` is pending, and clears that bit through
`Config.events_clear`. An unreadable events register conservatively reports a display change and a
failed clear is ignored, so the interrupt is never dropped from the IRQ path.

The code derives from `rcore-os/virtio-drivers` and keeps its MIT license (see `LICENSE`).

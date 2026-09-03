use crate::{CapsetInfo, DisplayError, DisplayInfo, DriverGeneric, FrameBuffer, TransferBox};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub handled: bool,
    pub changed: bool,
}

impl Event {
    pub const fn none() -> Self {
        Self {
            handled: false,
            changed: false,
        }
    }
}

pub trait Interface: DriverGeneric {
    // --- 2D methods ---

    fn info(&self) -> DisplayInfo;

    fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError>;

    fn irq_num(&self) -> Option<usize> {
        None
    }

    fn need_flush(&self) -> bool {
        false
    }

    fn flush(&mut self) -> Result<(), DisplayError> {
        Ok(())
    }

    fn enable_irq(&mut self) {}

    fn disable_irq(&mut self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn handle_irq(&mut self) -> Event {
        Event::none()
    }

    // --- 2D resource / scanout primitives (default: unsupported) ---

    /// Create a 2D resource with the given dimensions on the host.
    ///
    /// Maps to `VIRTIO_GPU_CMD_RESOURCE_CREATE_2D`. After creation, call
    /// [`resource_attach_backing`] to bind guest memory, then
    /// [`set_scanout`] to make it the display output.
    fn resource_create_2d(
        &mut self,
        _resource_id: u32,
        _width: u32,
        _height: u32,
    ) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Attach guest memory backing to a resource.
    ///
    /// Maps to `VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING`. This tells the
    /// host where the resource's guest-physical pages are so that
    /// `TRANSFER_TO_HOST_2D` / `TRANSFER_FROM_HOST_3D` can move data
    /// between guest RAM and the host resource.
    fn resource_attach_backing(
        &mut self,
        _resource_id: u32,
        _paddr: u64,
        _length: u32,
    ) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Bind a resource as the scanout (display output) for a given scanout ID.
    ///
    /// Maps to `VIRTIO_GPU_CMD_SET_SCANOUT`. Used for zero-copy display:
    /// a render target can be directly set as the scanout so the host
    /// displays it without a guest-side memcpy.
    fn set_scanout(
        &mut self,
        _scanout_id: u32,
        _resource_id: u32,
        _x: u32,
        _y: u32,
        _w: u32,
        _h: u32,
    ) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Transfer a rectangular region of a 2D resource from guest to host.
    ///
    /// Maps to `VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D`. This makes the host
    /// aware of guest-written pixel data before a [`resource_flush`].
    fn transfer_to_host_2d(
        &mut self,
        _resource_id: u32,
        _x: u32,
        _y: u32,
        _w: u32,
        _h: u32,
    ) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Flush a rectangular region of a resource to the display.
    ///
    /// Maps to `VIRTIO_GPU_CMD_RESOURCE_FLUSH`. After rendering into a
    /// resource and optionally binding it with [`set_scanout`], call
    /// this to make the host display the contents.
    fn resource_flush(
        &mut self,
        _resource_id: u32,
        _x: u32,
        _y: u32,
        _w: u32,
        _h: u32,
    ) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }

    // --- 3D methods (default: unsupported) ---

    /// Returns `true` if the device negotiated virgl 3D support.
    fn has_virgl(&self) -> bool {
        false
    }

    /// Returns `true` if `VIRTIO_GPU_F_RESOURCE_BLOB` was negotiated.
    ///
    /// Mesa decides whether to use the blob resource path from the
    /// `VIRTGPU_PARAM_RESOURCE_BLOB` GETPARAM value, which the kernel must
    /// report from this actual negotiation (Linux: `has_resource_blob`).
    fn has_resource_blob(&self) -> bool {
        false
    }

    /// Create a 3D rendering context.
    fn ctx_create(
        &mut self,
        _ctx_id: u32,
        _name: &str,
        _context_init: u32,
    ) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Destroy a 3D rendering context.
    fn ctx_destroy(&mut self, _ctx_id: u32) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Attach a 3D resource to a rendering context.
    ///
    /// A resource must be attached to a context before the context can use it
    /// for rendering commands. The resource must have been created first via
    /// [`resource_create_3d`].
    fn ctx_attach_resource(&mut self, _ctx_id: u32, _resource_id: u32) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Detach a 3D resource from a rendering context.
    ///
    /// Must be called before destroying a context if the context has attached
    /// resources. The host may not reclaim the resource until all contexts
    /// have detached it.
    fn ctx_detach_resource(&mut self, _ctx_id: u32, _resource_id: u32) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Create a 3D resource (texture, render target, buffer, etc.).
    ///
    /// The caller must explicitly call [`ctx_attach_resource`] after creation
    /// before using the resource in rendering commands.
    fn resource_create_3d(
        &mut self,
        ctx_id: u32,
        resource_id: u32,
        target: u32,
        format: u32,
        bind: u32,
        width: u32,
        height: u32,
        depth: u32,
        array_size: u32,
        last_level: u32,
        nr_samples: u32,
        flags: u32,
    ) -> Result<(), DisplayError> {
        let _ = (
            ctx_id,
            resource_id,
            target,
            format,
            bind,
            width,
            height,
            depth,
            array_size,
            last_level,
            nr_samples,
            flags,
        );
        Err(DisplayError::NotSupported)
    }

    /// Unreference (release) a 3D resource.
    fn resource_unref(&mut self, _resource_id: u32) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Create a blob resource (host-visible memory / dma-buf sharing).
    ///
    /// Maps to the Linux `virtio_gpu_resource_create_blob_ioctl`
    /// (`virtgpu_ioctl.c`). `blob_mem` is `VIRTIO_GPU_BLOB_MEM_GUEST (0x1)`,
    /// `HOST3D (0x2)` or `HOST3D_GUEST (0x3)`; `blob_flags` is the
    /// `VIRTIO_GPU_BLOB_FLAG_*` set.
    ///
    /// `cmd` is the virgl command stream for the blob's initial state (may be
    /// empty). To match Linux ordering, the implementation must submit `cmd`
    /// to the context *before* sending RESOURCE_CREATE_BLOB — both go on the
    /// same virtqueue in order. For HOST3D blobs `cmd` must be dword-aligned
    /// in size; for GUEST blobs it must be empty.
    fn resource_create_blob(
        &mut self,
        _ctx_id: u32,
        _resource_id: u32,
        _blob_mem: u32,
        _blob_flags: u32,
        _size: u64,
        _blob_id: u64,
        _cmd: &[u8],
    ) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Transfer data from guest to host for a 3D resource.
    fn transfer_to_host_3d(
        &mut self,
        ctx_id: u32,
        resource_id: u32,
        box_: TransferBox,
        offset: u64,
        level: u32,
        stride: u32,
        layer_stride: u32,
    ) -> Result<(), DisplayError> {
        let _ = (
            ctx_id,
            resource_id,
            box_,
            offset,
            level,
            stride,
            layer_stride,
        );
        Err(DisplayError::NotSupported)
    }

    /// Transfer data from host to guest for a 3D resource.
    fn transfer_from_host_3d(
        &mut self,
        ctx_id: u32,
        resource_id: u32,
        box_: TransferBox,
        offset: u64,
        level: u32,
        stride: u32,
        layer_stride: u32,
    ) -> Result<(), DisplayError> {
        let _ = (
            ctx_id,
            resource_id,
            box_,
            offset,
            level,
            stride,
            layer_stride,
        );
        Err(DisplayError::NotSupported)
    }

    /// Submit a virgl command buffer. Returns a monotonically increasing fence ID.
    fn submit_cmd(&mut self, _ctx_id: u32, _cmds: &[u8]) -> Result<u64, DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Block until the submit identified by `fence_id` (and everything enqueued
    /// before it) has completed on the host — the honest completion signal
    /// behind Linux `virtio_gpu_wait_ioctl` (`dma_resv_wait_timeout`).
    /// The fence ID comes from [`Interface::submit_cmd`].
    fn wait_fence(&mut self, _fence_id: u64) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Drain the completion queue (used ring) without blocking and without
    /// waiting for any specific fence.
    ///
    /// Called after fire-and-forget submits (EXECBUFFER/present) so the host's
    /// per-command completions are observed promptly: pumping advances the
    /// completion level and refreshes the device's notification watermark, so
    /// the next completion triggers the device IRQ immediately instead of
    /// being batched until a later pump (which delayed fence signaling by up
    /// to one frame — ~1.5ms on-screen, run54: ppoll avg 1465µs ≈ IRQ
    /// interval). Linux's virtio-gpu pumps in its completion worker after every
    /// IRQ, keeping per-command signal latency at µs.
    fn pump(&mut self) -> Result<(), DisplayError> {
        Ok(())
    }

    /// Non-blocking fence query: has `fence_id` already completed on the host?
    /// `false` means the host is still busy with the batch — Linux
    /// `dma_resv_test_signaled` (the NOWAIT probe in `virtio_gpu_wait_ioctl`).
    fn fence_completed(&mut self, _fence_id: u64) -> Result<bool, DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Completion-level-only fence query, **without** draining the used ring.
    ///
    /// For IRQ handlers that have already pumped (the display completion IRQ
    /// drains the ring before waking waiters): re-pumping per registered fence
    /// would double the per-IRQ cost. Defaults to the pumping variant so a
    /// backend without an IRQ path stays correct.
    fn fence_completed_no_pump(&mut self, fence_id: u64) -> Result<bool, DisplayError> {
        self.fence_completed(fence_id)
    }

    /// Query capset information by index.
    fn get_capset_info(&mut self, _index: u32) -> Result<CapsetInfo, DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Retrieve capset data.
    fn get_capset(
        &mut self,
        _id: u32,
        _ver: u32,
        _size: u32,
    ) -> Result<alloc::vec::Vec<u8>, DisplayError> {
        Err(DisplayError::NotSupported)
    }

    /// Flush any pending control-queue commands and notify the host — an
    /// ioctl/transaction boundary (Linux `virtio_gpu_notify()`, vq.c:551).
    ///
    /// Drivers that coalesce fire-and-forget control commands must deliver
    /// them with exactly one notify per transaction; call this once at the end
    /// of each ioctl that enqueued such commands. Default: no-op.
    fn ctrl_notify(&mut self) {}
}

//! The virtio-gpu control device.

use alloc::{boxed::Box, vec::Vec};
use core::mem::size_of;

use virtio_drivers::{
    BufferDirection, Hal, PAGE_SIZE,
    queue::VirtQueue,
    read_config,
    transport::{DeviceStatus, InterruptStatus, Transport},
    write_config,
};
use zerocopy::{FromBytes, Immutable, IntoBytes};

use crate::{
    BLOB_FLAG_USE_CROSS_DEVICE, BLOB_FLAG_USE_MASK, BLOB_MEM_GUEST, BLOB_MEM_HOST3D,
    BLOB_MEM_HOST3D_GUEST, CapsetInfo, Error, IrqEvent, Rect, ResourceCreate3d, ResourceCreateBlob,
    Transfer3d,
    dma::Dma,
    wire::{
        CmdCtxCreate, CmdCtxResource, CmdGetCapset, CmdGetCapsetInfo, CmdResourceCreate3D,
        CmdResourceCreateBlob, CmdSubmit3D, CmdTransferHost3D, Command, Config, CtrlHeader,
        Features, Format, MemEntry, ResourceAttachBacking, ResourceCreate2D, ResourceDetachBacking,
        ResourceFlush, ResourceUnref, RespCapsetInfo, RespDisplayInfo, SUPPORTED_FEATURES,
        SetScanout, TransferToHost2D, VIRTIO_GPU_EVENT_DISPLAY,
    },
};

/// Control queue index (the device also has a cursor queue, which this driver
/// does not use).
const CONTROL_QUEUE: u16 = 0;

/// Descriptors per virtqueue.
///
/// `SUBMIT_3D` puts three buffers into one chain (the `CtrlHeader`, the virgl
/// command stream and the response). `VirtQueue::add` requires the whole chain
/// to fit in the queue size, and 16 is the smallest power of two that covers
/// that plus room for pipelined commands.
const CONTROL_QUEUE_SIZE: u16 = 16;

/// Receive buffer for control responses.
const RECV_BUF_SIZE: usize = PAGE_SIZE;

/// Transmit buffer for control requests.
const SEND_BUF_SIZE: usize = PAGE_SIZE;

/// Scanout driven by this driver.
const SCANOUT_ID: u32 = 0;

/// Resource backing the default framebuffer.
const FRAMEBUFFER_RESOURCE_ID: u32 = 0xbabe;

/// A virtio-gpu device driven over any [`Transport`].
///
/// The driver covers the 2D display path and the virgl 3D path. Capabilities
/// that the device did not negotiate are rejected with [`Error::Unsupported`]
/// instead of being sent anyway.
pub struct VirtIoGpu<H: Hal, T: Transport> {
    transport: T,
    /// Rectangle of the current framebuffer, if one was set up.
    rect: Option<Rect>,
    /// DMA region backing the default framebuffer.
    frame_buffer_dma: Option<Dma<H>>,
    /// Queue carrying control commands.
    control_queue: VirtQueue<H, { CONTROL_QUEUE_SIZE as usize }>,
    /// Receive buffer for control responses.
    queue_buf_recv: Box<[u8]>,
    /// Transmit buffer for control requests.
    ///
    /// Control requests must live in memory that stays mapped and unchanged for
    /// as long as the command is in flight: the device reads the request header
    /// asynchronously after the descriptor is published, so the buffer is owned
    /// by this structure for its whole lifetime instead of pointing at a
    /// by-value request that would go out of scope while the device still reads
    /// it. See [`VirtIoGpu::request_with_len`].
    queue_buf_send: Box<[u8]>,
    /// Whether the VIRGL 3D feature was negotiated.
    has_virgl: bool,
    /// Whether `VIRTIO_GPU_F_RESOURCE_BLOB` was negotiated.
    has_resource_blob: bool,
    /// Whether `VIRTIO_GPU_F_CONTEXT_INIT` was negotiated.
    has_context_init: bool,
}

impl<H: Hal, T: Transport> VirtIoGpu<H, T> {
    /// Initialises the device over `transport` and negotiates the feature set.
    pub fn new(mut transport: T) -> Result<Self, Error> {
        let negotiated = transport.begin_init(SUPPORTED_FEATURES);

        let events_read = read_config!(transport, Config, events_read)?;
        let num_scanouts = read_config!(transport, Config, num_scanouts)?;
        log::debug!(
            "virtio-gpu config: events_read={events_read:#x}, num_scanouts={num_scanouts:#x}"
        );

        let control_queue = VirtQueue::new(
            &mut transport,
            CONTROL_QUEUE,
            negotiated.contains(Features::RING_INDIRECT_DESC),
            negotiated.contains(Features::RING_EVENT_IDX),
        )?;

        let queue_buf_recv = alloc::vec![0u8; RECV_BUF_SIZE].into_boxed_slice();
        let queue_buf_send = alloc::vec![0u8; SEND_BUF_SIZE].into_boxed_slice();

        transport.finish_init();

        let has_virgl = negotiated.contains(Features::VIRGL);
        let has_resource_blob = negotiated.contains(Features::RESOURCE_BLOB);
        let has_context_init = negotiated.contains(Features::CONTEXT_INIT);
        log::info!(
            "virtio-gpu features: negotiated={negotiated:?}, has_virgl={has_virgl}, \
             has_resource_blob={has_resource_blob}, has_context_init={has_context_init}"
        );

        Ok(Self {
            transport,
            rect: None,
            frame_buffer_dma: None,
            control_queue,
            queue_buf_recv,
            queue_buf_send,
            has_virgl,
            has_resource_blob,
            has_context_init,
        })
    }

    /// Acknowledges the pending interrupt and reports what it carried.
    ///
    /// A device-configuration interrupt is read out of the config space, as
    /// Linux does in `virtio_gpu_config_changed_work_func()`: only the pending
    /// `VIRTIO_GPU_EVENT_DISPLAY` bit sets `display_changed`, and it is cleared
    /// through `events_clear`. Config-space reads and writes cannot fail the
    /// interrupt: an unreadable `events_read` conservatively reports
    /// `display_changed`, a failed clear is ignored, and the configuration
    /// interrupt stays reported as handled either way, because the status bit
    /// was already acknowledged.
    pub fn ack_interrupt(&mut self) -> IrqEvent {
        let status = self.transport.ack_interrupt();
        let queue = status.contains(InterruptStatus::QUEUE_INTERRUPT);
        let configuration = status.contains(InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT);
        let mut display_changed = false;
        if configuration {
            match read_config!(self.transport, Config, events_read) {
                Ok(events) => {
                    if events & VIRTIO_GPU_EVENT_DISPLAY != 0 {
                        display_changed = true;
                        // A failed clear is not fatal: the event stays
                        // reported and a later interrupt re-reads the same
                        // register.
                        let _ = write_config!(
                            self.transport,
                            Config,
                            events_clear,
                            VIRTIO_GPU_EVENT_DISPLAY
                        );
                    }
                }
                // The events register is unreadable, so the pending event
                // cannot be classified. Report a display change rather than
                // dropping the interrupt.
                Err(_) => display_changed = true,
            }
        }
        IrqEvent {
            queue,
            configuration,
            display_changed,
        }
    }

    /// Returns the device's current display resolution in pixels.
    pub fn resolution(&mut self) -> Result<(u32, u32), Error> {
        let info = self.display_info()?;
        Ok((info.rect.width, info.rect.height))
    }

    /// Sets up a framebuffer at the device's preferred resolution.
    ///
    /// See [`VirtIoGpu::change_resolution`] for the validity of the returned
    /// slice.
    pub fn setup_framebuffer(&mut self) -> Result<&mut [u8], Error> {
        let info = self.display_info()?;
        self.change_resolution(info.rect.width, info.rect.height)
    }

    /// Recreates the framebuffer resource with the given size and binds it to
    /// the scanout.
    ///
    /// An existing framebuffer is torn down first, telling the device to stop
    /// using its backing before that backing is released. The returned slice
    /// borrows `self`, so the device's backing stays allocated and exclusively
    /// reachable for as long as the slice is alive.
    ///
    /// The framebuffer is only published (as `rect` and `frame_buffer_dma`)
    /// after the resource is created, its backing is attached and the scanout
    /// is bound, so a failure part-way leaves no state claiming a framebuffer
    /// that the device is not actually scanning out.
    pub fn change_resolution(&mut self, width: u32, height: u32) -> Result<&mut [u8], Error> {
        let rect = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let size = framebuffer_size(width, height)?;

        // Stop any existing framebuffer before touching the device again. The
        // teardown keeps the old DMA in `self.frame_buffer_dma` on failure so
        // the device never keeps a pointer to memory that has been freed.
        if self.frame_buffer_dma.is_some() {
            self.teardown_framebuffer()?;
        }

        // Allocate the backing before creating the host resource. A DMA
        // allocation failure must not leave a resource behind that nothing
        // references, and creating the resource only after the allocation
        // succeeds makes the opposite order impossible.
        let frame_buffer_dma = Dma::new(size as usize, BufferDirection::DriverToDevice)?;
        let paddr = frame_buffer_dma.paddr();
        let mut raw = frame_buffer_dma.raw_slice();

        // Create the resource. If this fails the DMA drops here (the device has
        // never seen it) and there is nothing to roll back.
        self.resource_create_2d(FRAMEBUFFER_RESOURCE_ID, width, height)?;

        // SAFETY: `frame_buffer_dma` owns a live, zeroed, at least `size` byte
        // DMA region (the allocation is rounded up to whole pages). On the
        // success path the value is moved into `self.frame_buffer_dma`, and it
        // is only released by the teardown path after `resource_detach_backing`
        // and `resource_unref`, so the device stops using the range before it
        // is freed. Nothing else in this driver hands the same range to the
        // device or aliases it.
        if let Err(err) =
            unsafe { self.resource_attach_backing(FRAMEBUFFER_RESOURCE_ID, paddr, size) }
        {
            // The resource exists but has no backing; release it so a failed
            // attach does not leak a host resource. The DMA drops here because
            // the device never received the range.
            let _ = self.resource_unref(FRAMEBUFFER_RESOURCE_ID);
            return Err(err);
        }

        // Bind the resource to the scanout. If that fails we must stop the
        // device from using the backing before freeing it: detach first, then
        // unref. Only a confirmed detach lets the DMA be released; otherwise it
        // is kept in `self.frame_buffer_dma` so a later retry, or the `Drop`
        // device reset, can release it once the device is known to be done.
        if let Err(err) = self.set_scanout(rect, SCANOUT_ID, FRAMEBUFFER_RESOURCE_ID) {
            let detached = self
                .resource_detach_backing(FRAMEBUFFER_RESOURCE_ID)
                .is_ok();
            let _ = self.resource_unref(FRAMEBUFFER_RESOURCE_ID);
            if !detached {
                // Keep the DMA alive: the device may still be writing into it.
                self.frame_buffer_dma = Some(frame_buffer_dma);
            }
            return Err(err);
        }

        // SAFETY: `raw` points at the allocation owned by `frame_buffer_dma`,
        // which is moved into `self` below and kept until the teardown path
        // detaches and unreferences the resource, so the memory outlives the
        // returned slice. The slice borrows `self` mutably, so no other
        // reference to the framebuffer can exist while it is alive.
        let buffer: &mut [u8] = unsafe { raw.as_mut() };

        self.frame_buffer_dma = Some(frame_buffer_dma);
        self.rect = Some(rect);
        Ok(buffer)
    }

    /// Stops the device from using the current framebuffer and releases its
    /// backing.
    ///
    /// The scanout is stopped first, then the backing is detached and the
    /// resource released, and only then is the DMA freed (dropping the value
    /// stored in `self.frame_buffer_dma`). On any failure the DMA stays owned
    /// by `self.frame_buffer_dma`, so the device never keeps a pointer to freed
    /// memory and a later retry can finish the teardown.
    fn teardown_framebuffer(&mut self) -> Result<(), Error> {
        self.set_scanout(Rect::default(), SCANOUT_ID, 0)?;
        // The scanout is stopped, so any previously published framebuffer is
        // no longer live.
        self.rect = None;
        self.resource_detach_backing(FRAMEBUFFER_RESOURCE_ID)?;
        self.resource_unref(FRAMEBUFFER_RESOURCE_ID)?;
        self.frame_buffer_dma = None;
        Ok(())
    }

    /// Bind the driver's own 2D framebuffer after another scanout was in use.
    pub fn restore_framebuffer_scanout(&mut self) -> Result<(), Error> {
        let rect = self.rect.ok_or(Error::NotReady)?;
        self.set_scanout(rect, SCANOUT_ID, FRAMEBUFFER_RESOURCE_ID)
    }

    /// Transfers the framebuffer to the host and flushes it to the scanout.
    pub fn flush(&mut self) -> Result<(), Error> {
        let rect = self.rect.ok_or(Error::NotReady)?;
        self.transfer_to_host_2d(rect, 0, FRAMEBUFFER_RESOURCE_ID)?;
        self.resource_flush(rect, FRAMEBUFFER_RESOURCE_ID)
    }

    // --- 2D resource and scanout commands ---

    /// Creates a 2D resource in `B8G8R8A8_UNORM` format.
    pub fn resource_create_2d(
        &mut self,
        resource_id: u32,
        width: u32,
        height: u32,
    ) -> Result<(), Error> {
        let response: CtrlHeader = self.request(ResourceCreate2D {
            header: CtrlHeader::with_type(Command::RESOURCE_CREATE_2D),
            resource_id,
            format: Format::B8G8R8A8Unorm,
            width,
            height,
        })?;
        response.check_type(Command::OK_NODATA)
    }

    /// Binds `resource_id` to `scanout_id` for the given display area.
    pub fn set_scanout(
        &mut self,
        rect: Rect,
        scanout_id: u32,
        resource_id: u32,
    ) -> Result<(), Error> {
        let response: CtrlHeader = self.request(SetScanout {
            header: CtrlHeader::with_type(Command::SET_SCANOUT),
            rect,
            scanout_id,
            resource_id,
        })?;
        response.check_type(Command::OK_NODATA)
    }

    /// Refreshes `rect` of `resource_id` on the display.
    pub fn resource_flush(&mut self, rect: Rect, resource_id: u32) -> Result<(), Error> {
        let response: CtrlHeader = self.request(ResourceFlush {
            header: CtrlHeader::with_type(Command::RESOURCE_FLUSH),
            rect,
            resource_id,
            _padding: 0,
        })?;
        response.check_type(Command::OK_NODATA)
    }

    /// Transfers `rect` of a 2D resource from guest memory to the host.
    pub fn transfer_to_host_2d(
        &mut self,
        rect: Rect,
        offset: u64,
        resource_id: u32,
    ) -> Result<(), Error> {
        let response: CtrlHeader = self.request(TransferToHost2D {
            header: CtrlHeader::with_type(Command::TRANSFER_TO_HOST_2D),
            rect,
            offset,
            resource_id,
            _padding: 0,
        })?;
        response.check_type(Command::OK_NODATA)
    }

    /// Attaches one guest-physical memory range to a resource.
    ///
    /// The device reads and writes `paddr..paddr + length` directly for as long
    /// as the resource exists, so the caller must guarantee that the range is
    /// device-accessible guest memory and stays allocated, unaliased and free of
    /// concurrent access until the matching [`VirtIoGpu::resource_unref`]
    /// (after a [`VirtIoGpu::resource_detach_backing`], when the caller drives
    /// the teardown itself).
    ///
    /// A zero `length` is rejected with [`Error::InvalidParam`] and a range that
    /// would wrap the 64-bit address space with [`Error::Overflow`], before
    /// anything is sent. These are protocol and arithmetic checks only; they do
    /// not weaken the ownership contract below.
    ///
    /// # Safety
    ///
    /// `paddr..paddr + length` must be valid device-accessible memory that
    /// outlives this mapping and is not accessed by anyone else while the device
    /// may touch it. `length` must not exceed the region actually owned by the
    /// caller.
    pub unsafe fn resource_attach_backing(
        &mut self,
        resource_id: u32,
        paddr: u64,
        length: u32,
    ) -> Result<(), Error> {
        if length == 0 {
            return Err(Error::InvalidParam);
        }
        // The device walks `paddr..paddr + length`; a wrapped extent would hand
        // it a range that has nothing to do with the caller's allocation.
        paddr
            .checked_add(u64::from(length))
            .ok_or(Error::Overflow)?;
        let response: CtrlHeader = self.request(ResourceAttachBacking {
            header: CtrlHeader::with_type(Command::RESOURCE_ATTACH_BACKING),
            resource_id,
            nr_entries: 1,
            addr: paddr,
            length,
            _padding: 0,
        })?;
        response.check_type(Command::OK_NODATA)
    }

    /// Detaches the backing memory from a resource.
    ///
    /// After this returns, the device no longer reads or writes the ranges that
    /// were attached.
    fn resource_detach_backing(&mut self, resource_id: u32) -> Result<(), Error> {
        let response: CtrlHeader = self.request(ResourceDetachBacking {
            header: CtrlHeader::with_type(Command::RESOURCE_DETACH_BACKING),
            resource_id,
            _padding: 0,
        })?;
        response.check_type(Command::OK_NODATA)
    }

    /// Releases a resource.
    ///
    /// The protocol has a single `RESOURCE_UNREF` for 2D and 3D resources, so
    /// this is also the only way to destroy a 3D resource. Once it returns, the
    /// guest memory previously attached to the resource may be freed by its
    /// owner.
    pub fn resource_unref(&mut self, resource_id: u32) -> Result<(), Error> {
        let response: CtrlHeader = self.request(ResourceUnref {
            header: CtrlHeader::with_type(Command::RESOURCE_UNREF),
            resource_id,
            _padding: 0,
        })?;
        response.check_type(Command::OK_NODATA)
    }

    // --- 3D (virgl) commands ---

    /// Whether the device negotiated virgl 3D support.
    pub fn has_virgl(&self) -> bool {
        self.has_virgl
    }

    /// Whether the device negotiated `VIRTIO_GPU_F_RESOURCE_BLOB`.
    ///
    /// Blob resources are what makes host-visible memory and dma-buf sharing
    /// (PRIME) possible. Note that a plain `GUEST` blob only needs this
    /// feature, not VIRGL; only `HOST3D` blobs need a rendering context.
    pub fn has_resource_blob(&self) -> bool {
        self.has_resource_blob
    }

    /// Whether the device negotiated `VIRTIO_GPU_F_CONTEXT_INIT`.
    ///
    /// Linux reports this as `has_context_init` in
    /// `virtgpu_getparam_ioctl()`; Mesa uses it to decide between the
    /// context-init protocol (a capset ID in `CTX_CREATE`) and the legacy
    /// VIRGL path. This is the actual negotiation result and is independent of
    /// `has_virgl`.
    pub fn has_context_init(&self) -> bool {
        self.has_context_init
    }

    /// Rejects 3D commands when virgl was not negotiated.
    ///
    /// Every 3D command is undefined without the feature and the host would
    /// reject it, so the driver fails early with a domain error.
    fn require_virgl(&self) -> Result<(), Error> {
        if self.has_virgl {
            Ok(())
        } else {
            Err(Error::Unsupported)
        }
    }

    /// Queries capset metadata by index, starting at 0.
    pub fn get_capset_info(&mut self, capset_index: u32) -> Result<CapsetInfo, Error> {
        self.require_virgl()?;
        let response: RespCapsetInfo = self.request(CmdGetCapsetInfo {
            header: CtrlHeader::with_type(Command::GET_CAPSET_INFO),
            capset_index,
            _padding: 0,
        })?;
        response.header.check_type(Command::OK_CAPSET_INFO)?;
        Ok(CapsetInfo {
            capset_id: response.capset_id,
            max_version: response.capset_max_version,
            max_size: response.capset_max_size,
        })
    }

    /// Retrieves the capset data for `capset_id` and `version`.
    ///
    /// `size` is the upper bound from [`VirtIoGpu::get_capset_info`]. Only the
    /// bytes the device actually wrote are returned, and a `size` that would not
    /// fit the receive buffer is rejected with [`Error::ResponseTooLarge`]
    /// rather than truncated.
    pub fn get_capset(
        &mut self,
        capset_id: u32,
        version: u32,
        size: u32,
    ) -> Result<Vec<u8>, Error> {
        self.require_virgl()?;
        let header_len = size_of::<CtrlHeader>();
        let capacity = self.queue_buf_recv.len().saturating_sub(header_len);
        if size as usize > capacity {
            return Err(Error::ResponseTooLarge);
        }

        let (header, used_len): (CtrlHeader, usize) = self.request_with_len(CmdGetCapset {
            header: CtrlHeader::with_type(Command::GET_CAPSET),
            capset_id,
            capset_version: version,
        })?;
        header.check_type(Command::OK_CAPSET)?;

        // `size` is only an upper bound: slice by the bytes the device actually
        // wrote so stale receive-buffer contents never leak into the blob.
        let start = header_len;
        let end = used_len.clamp(start, start + size as usize);
        Ok(self.queue_buf_recv[start..end].to_vec())
    }

    /// Creates a 3D rendering context.
    ///
    /// `context_init` carries the capset ID that selects the context protocol
    /// (0 for virgl1, 2 for virgl2). `name` is a debug label the host may show;
    /// it is truncated to 64 bytes.
    ///
    /// The context-init protocol is only available when the device negotiated
    /// `VIRTIO_GPU_F_CONTEXT_INIT`, so a non-zero `context_init` on a device
    /// without it is rejected with [`Error::Unsupported`] instead of being sent.
    /// The legacy `context_init == 0` path still works whenever VIRGL was
    /// negotiated.
    pub fn ctx_create(&mut self, ctx_id: u32, name: &str, context_init: u32) -> Result<(), Error> {
        self.require_virgl()?;
        if context_init != 0 && !self.has_context_init {
            return Err(Error::Unsupported);
        }
        let mut cmd = CmdCtxCreate {
            header: CtrlHeader::with_type_and_ctx(Command::CTX_CREATE, ctx_id),
            nlen: 0,
            context_init,
            debug_name: [0u8; 64],
        };
        let bytes = name.as_bytes();
        let nlen = bytes.len().min(cmd.debug_name.len());
        cmd.debug_name[..nlen].copy_from_slice(&bytes[..nlen]);
        cmd.nlen = nlen as u32;

        let response: CtrlHeader = self.request(cmd)?;
        response.check_type(Command::OK_NODATA)
    }

    /// Destroys a 3D rendering context.
    pub fn ctx_destroy(&mut self, ctx_id: u32) -> Result<(), Error> {
        self.require_virgl()?;
        let response: CtrlHeader =
            self.request(CtrlHeader::with_type_and_ctx(Command::CTX_DESTROY, ctx_id))?;
        response.check_type(Command::OK_NODATA)
    }

    /// Attaches a resource to a rendering context.
    pub fn ctx_attach_resource(&mut self, ctx_id: u32, resource_id: u32) -> Result<(), Error> {
        self.require_virgl()?;
        let response: CtrlHeader = self.request(CmdCtxResource {
            header: CtrlHeader::with_type_and_ctx(Command::CTX_ATTACH_RESOURCE, ctx_id),
            resource_id,
            _padding: 0,
        })?;
        response.check_type(Command::OK_NODATA)
    }

    /// Detaches a resource from a rendering context.
    pub fn ctx_detach_resource(&mut self, ctx_id: u32, resource_id: u32) -> Result<(), Error> {
        self.require_virgl()?;
        let response: CtrlHeader = self.request(CmdCtxResource {
            header: CtrlHeader::with_type_and_ctx(Command::CTX_DETACH_RESOURCE, ctx_id),
            resource_id,
            _padding: 0,
        })?;
        response.check_type(Command::OK_NODATA)
    }

    /// Creates a 3D resource such as a texture, render target or buffer.
    pub fn resource_create_3d(&mut self, params: ResourceCreate3d) -> Result<(), Error> {
        self.require_virgl()?;
        let response: CtrlHeader = self.request(CmdResourceCreate3D {
            header: CtrlHeader::with_type_and_ctx(Command::RESOURCE_CREATE_3D, params.ctx_id),
            resource_id: params.resource_id,
            target: params.target,
            format: params.format,
            bind: params.bind,
            width: params.width,
            height: params.height,
            depth: params.depth,
            array_size: params.array_size,
            last_level: params.last_level,
            nr_samples: params.nr_samples,
            flags: params.flags,
            _padding: 0,
        })?;
        response.check_type(Command::OK_NODATA)
    }

    /// Transfers a 3D resource from guest memory to the host.
    pub fn transfer_to_host_3d(&mut self, params: Transfer3d) -> Result<(), Error> {
        self.require_virgl()?;
        let response: CtrlHeader = self.request(CmdTransferHost3D {
            header: CtrlHeader::with_type_and_ctx(Command::TRANSFER_TO_HOST_3D, params.ctx_id),
            box_: params.box_,
            offset: params.offset,
            resource_id: params.resource_id,
            level: params.level,
            stride: params.stride,
            layer_stride: params.layer_stride,
        })?;
        response.check_type(Command::OK_NODATA)
    }

    /// Transfers a 3D resource from the host to guest memory.
    pub fn transfer_from_host_3d(&mut self, params: Transfer3d) -> Result<(), Error> {
        self.require_virgl()?;
        let response: CtrlHeader = self.request(CmdTransferHost3D {
            header: CtrlHeader::with_type_and_ctx(Command::TRANSFER_FROM_HOST_3D, params.ctx_id),
            box_: params.box_,
            offset: params.offset,
            resource_id: params.resource_id,
            level: params.level,
            stride: params.stride,
            layer_stride: params.layer_stride,
        })?;
        response.check_type(Command::OK_NODATA)
    }

    /// Submits a virgl command stream to a rendering context.
    ///
    /// `cmds` is the encoded stream produced by the Mesa virgl Gallium driver
    /// in userspace and is sent as a second buffer next to the `SUBMIT_3D`
    /// header. `fence_id` is assigned by the caller; the host signals that fence
    /// once the stream has been processed. The stream length must be a multiple
    /// of four, because the host passes `size / 4` dwords to virglrenderer.
    pub fn submit_3d(&mut self, ctx_id: u32, fence_id: u64, cmds: &[u8]) -> Result<(), Error> {
        self.require_virgl()?;
        if !cmds.len().is_multiple_of(size_of::<u32>()) {
            return Err(Error::InvalidParam);
        }
        let size = u32::try_from(cmds.len()).map_err(|_| Error::Overflow)?;
        let response: CtrlHeader = self.request_with_data(
            CmdSubmit3D {
                header: CtrlHeader::with_fence(Command::SUBMIT_3D, ctx_id, fence_id),
                size,
                _padding: 0,
            },
            cmds,
        )?;
        response.check_type(Command::OK_NODATA)
    }

    /// Creates a blob resource such as host-visible memory or a dma-buf.
    ///
    /// A plain `GUEST` blob only needs `VIRTIO_GPU_F_RESOURCE_BLOB`; `HOST3D`
    /// and `HOST3D_GUEST` blobs additionally need VIRGL, because the host 3D
    /// memory they name belongs to a rendering context.
    ///
    /// The parameters are validated before anything is sent:
    ///
    /// * `blob_mem` must be one of [`BLOB_MEM_GUEST`], [`BLOB_MEM_HOST3D`] or
    ///   [`BLOB_MEM_HOST3D_GUEST`];
    /// * `size` must be non-zero;
    /// * `blob_flags` may only set the low three, defined
    ///   `VIRTGPU_BLOB_FLAG_USE_*` bits, and cross-device blobs are rejected
    ///   because the required UUID feature is not negotiated ([`Error::Unsupported`]);
    /// * `GUEST` and `HOST3D_GUEST` blobs must pass a non-empty `mem_entries`
    ///   whose lengths are each non-zero, whose `paddr + length` ranges do not
    ///   wrap the 64-bit address space, and whose lengths sum (checked) to at
    ///   least `size`;
    /// * `HOST3D` blobs must pass no guest backing at all.
    ///
    /// # Safety
    ///
    /// For `GUEST` and `HOST3D_GUEST` blobs the device reads and writes the
    /// guest memory at the addresses in [`ResourceCreateBlob::mem_entries`].
    /// The caller must guarantee that every range is valid device-accessible
    /// memory and stays allocated and free of concurrent access for as long as
    /// the blob resource exists, that is until the matching
    /// [`VirtIoGpu::resource_unref`]. The ranges must also cover `size` bytes in
    /// total: a blob larger than its backing would let the device reach past the
    /// end of the provided ranges. `HOST3D` blobs must pass no entries at all.
    pub unsafe fn resource_create_blob(
        &mut self,
        params: ResourceCreateBlob<'_>,
    ) -> Result<(), Error> {
        if !self.has_resource_blob {
            return Err(Error::Unsupported);
        }

        // Only the low three `VIRTGPU_BLOB_FLAG_USE_*` bits are defined; any
        // other bit is a caller bug, not something the device should see.
        if params.blob_flags & !BLOB_FLAG_USE_MASK != 0 {
            return Err(Error::InvalidParam);
        }
        // This crate neither negotiates `VIRTIO_GPU_F_RESOURCE_UUID` nor
        // implements `RESOURCE_ASSIGN_UUID`, so a cross-device blob cannot be
        // honoured. Reject it instead of forwarding a flag the device would
        // accept without the guest ever being able to name the host blob.
        if params.blob_flags & BLOB_FLAG_USE_CROSS_DEVICE != 0 {
            return Err(Error::Unsupported);
        }
        if params.size == 0 {
            return Err(Error::InvalidParam);
        }

        let guest_backed = match params.blob_mem {
            BLOB_MEM_GUEST => true,
            BLOB_MEM_HOST3D_GUEST => true,
            BLOB_MEM_HOST3D => false,
            _ => return Err(Error::InvalidParam),
        };
        let host3d = matches!(params.blob_mem, BLOB_MEM_HOST3D | BLOB_MEM_HOST3D_GUEST);
        if host3d && !self.has_virgl {
            return Err(Error::Unsupported);
        }

        if guest_backed {
            let mut total: u64 = 0;
            for entry in params.mem_entries {
                if entry.length == 0 {
                    return Err(Error::InvalidParam);
                }
                // The device reads and writes `paddr..paddr + length`; a wrapped
                // extent would name a range unrelated to the caller's backing.
                entry
                    .paddr
                    .checked_add(u64::from(entry.length))
                    .ok_or(Error::Overflow)?;
                total = total
                    .checked_add(u64::from(entry.length))
                    .ok_or(Error::Overflow)?;
            }
            // A blob larger than its backing would let the device reach past
            // the end of the provided ranges, so require the ranges to cover
            // `size` bytes. An empty slice sums to zero and fails this check.
            if total < params.size {
                return Err(Error::InvalidParam);
            }
        } else if !params.mem_entries.is_empty() {
            // `HOST3D` blobs are backed by host memory and must carry no guest
            // ranges; virglrenderer rejects a nonzero `num_iovs`.
            return Err(Error::InvalidParam);
        }

        let nr_entries = u32::try_from(params.mem_entries.len()).map_err(|_| Error::Overflow)?;
        let capacity = params
            .mem_entries
            .len()
            .checked_mul(size_of::<MemEntry>())
            .ok_or(Error::Overflow)?;
        let mut data = Vec::with_capacity(capacity);
        for entry in params.mem_entries {
            data.extend_from_slice(
                MemEntry {
                    addr: entry.paddr,
                    length: entry.length,
                    padding: 0,
                }
                .as_bytes(),
            );
        }

        let response: CtrlHeader = self.request_with_data(
            CmdResourceCreateBlob {
                header: CtrlHeader::with_type_and_ctx(Command::RESOURCE_CREATE_BLOB, params.ctx_id),
                resource_id: params.resource_id,
                blob_mem: params.blob_mem,
                blob_flags: params.blob_flags,
                nr_entries,
                blob_id: params.blob_id,
                size: params.size,
            },
            &data,
        )?;
        response.check_type(Command::OK_NODATA)
    }

    // --- Command plumbing ---

    /// Sends a command and returns the parsed response.
    fn request<Req, Rsp>(&mut self, req: Req) -> Result<Rsp, Error>
    where
        Req: IntoBytes + Immutable,
        Rsp: FromBytes,
    {
        Ok(self.request_with_len(req)?.0)
    }

    /// Like [`VirtIoGpu::request`], but also reports how many bytes the device
    /// wrote into the receive buffer.
    ///
    /// The request is copied into the long-lived control send buffer before it
    /// is submitted, and only the bytes of the request are handed to the queue:
    /// a by-value request would live on the stack, which is neither guaranteed
    /// to stay mapped for the device nor stable while `add_notify_wait_pop`
    /// waits, and exposing the whole page would present the untouched tail as
    /// part of the command.
    fn request_with_len<Req, Rsp>(&mut self, req: Req) -> Result<(Rsp, usize), Error>
    where
        Req: IntoBytes + Immutable,
        Rsp: FromBytes,
    {
        let req_len = copy_request_into(&mut self.queue_buf_send, &req)?;
        let used_len = self.control_queue.add_notify_wait_pop(
            &[&self.queue_buf_send[..req_len]],
            &mut [&mut self.queue_buf_recv],
            &mut self.transport,
        )? as usize;
        let response = self.parse_response(used_len)?;
        Ok((response, used_len))
    }

    /// Sends a command with a second device-readable buffer, used by `SUBMIT_3D`
    /// and `RESOURCE_CREATE_BLOB`.
    ///
    /// The chain is ordered request header, caller data, response. Like
    /// [`VirtIoGpu::request_with_len`], the header is copied into the control
    /// send buffer and only its exact length is submitted. `data` is borrowed
    /// by the virtqueue, so the caller's slice must stay allocated and unchanged
    /// until this call returns, which the signature enforces.
    fn request_with_data<Req, Rsp>(&mut self, req: Req, data: &[u8]) -> Result<Rsp, Error>
    where
        Req: IntoBytes + Immutable,
        Rsp: FromBytes,
    {
        let req_len = copy_request_into(&mut self.queue_buf_send, &req)?;
        let inputs: &[&[u8]] = if data.is_empty() {
            &[&self.queue_buf_send[..req_len]]
        } else {
            &[&self.queue_buf_send[..req_len], data]
        };
        let used_len = self.control_queue.add_notify_wait_pop(
            inputs,
            &mut [&mut self.queue_buf_recv],
            &mut self.transport,
        )? as usize;
        self.parse_response(used_len)
    }

    /// Validates the response length and parses `Rsp` from exactly the bytes the
    /// device wrote.
    ///
    /// `used_len` is the number of bytes the device reported for the response.
    /// It must fit in the receive buffer and be large enough to hold `Rsp`; a
    /// larger or smaller value is rejected before any parsing, so stale bytes
    /// left over in the receive buffer are never interpreted as a response.
    fn parse_response<Rsp: FromBytes>(&self, used_len: usize) -> Result<Rsp, Error> {
        if used_len > self.queue_buf_recv.len() {
            return Err(Error::ResponseTooLarge);
        }
        if used_len < size_of::<Rsp>() {
            return Err(Error::InvalidResponse);
        }
        let (response, _) = Rsp::read_from_prefix(&self.queue_buf_recv[..used_len])
            .map_err(|_| Error::InvalidResponse)?;
        Ok(response)
    }

    /// `GET_DISPLAY_INFO`, validated.
    fn display_info(&mut self) -> Result<RespDisplayInfo, Error> {
        let info: RespDisplayInfo =
            self.request(CtrlHeader::with_type(Command::GET_DISPLAY_INFO))?;
        info.header.check_type(Command::OK_DISPLAY_INFO)?;
        Ok(info)
    }
}

impl<H: Hal, T: Transport> Drop for VirtIoGpu<H, T> {
    fn drop(&mut self) {
        // Reset the device before any field is released. Writing an empty
        // status tells the device to drop its driver state, which stops the
        // scanout and tears down the host-side resource backing, so the device
        // stops issuing DMA. Without this the device could keep scanning out of
        // (or writing into) the framebuffer DMA that `frame_buffer_dma` frees
        // when the fields below are dropped, a use-after-free from the device's
        // point of view. A status write is a single register or PCI capability
        // write, so it cannot block on the control queue; no control command is
        // sent here.
        self.transport.set_status(DeviceStatus::empty());
        // Clear the queue registration so the device cannot keep reading the
        // descriptor rings after the transport and its DMA are released.
        self.transport.queue_unset(CONTROL_QUEUE);
    }
}

/// Size in bytes of a `width * height` `B8G8R8A8_UNORM` framebuffer.
fn framebuffer_size(width: u32, height: u32) -> Result<u32, Error> {
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(Error::Overflow)
}

/// Copies `req` into `buf` and returns the length of the request, which is the
/// number of leading bytes of `buf` that carry it.
///
/// The single size check lives here so both control-queue entry points reject an
/// oversized request with [`Error::RequestTooLarge`] instead of truncating it.
fn copy_request_into<Req: IntoBytes + Immutable>(
    buf: &mut [u8],
    req: &Req,
) -> Result<usize, Error> {
    let bytes = req.as_bytes();
    if bytes.len() > buf.len() {
        return Err(Error::RequestTooLarge);
    }
    buf[..bytes.len()].copy_from_slice(bytes);
    Ok(bytes.len())
}

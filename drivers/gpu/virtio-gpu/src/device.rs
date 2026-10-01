//! The virtio-gpu control device.

use alloc::{boxed::Box, vec::Vec};
use core::mem::size_of;

use virtio_drivers::{
    BufferDirection, Hal, PAGE_SIZE, read_config,
    transport::{DeviceStatus, InterruptStatus, Transport},
    write_config,
};
use zerocopy::{FromBytes, Immutable, IntoBytes};

use crate::{
    BLOB_FLAG_USE_CROSS_DEVICE, BLOB_FLAG_USE_MASK, BLOB_MEM_GUEST, BLOB_MEM_HOST3D,
    BLOB_MEM_HOST3D_GUEST, BlobMemory, CapsetInfo, Error, IrqEvent, OutputInfo, Rect,
    Resource2dFormat, ResourceCreate3d, ResourceCreateBlob, Transfer3d,
    ctrl::ControlQueue,
    dma::Dma,
    wire::{
        CmdCtxCreate, CmdCtxResource, CmdGetCapset, CmdGetCapsetInfo, CmdResourceCreate3D,
        CmdResourceCreateBlob, CmdSubmit3D, CmdTransferHost3D, Command, Config, CtrlHeader,
        Features, GPU_FLAG_FENCE, MemEntry, ResourceAttachBacking, ResourceCreate2D,
        ResourceDetachBacking, ResourceFlush, ResourceUnref, RespCapsetInfo, RespDisplayInfo,
        SUPPORTED_FEATURES, SetScanout, SetScanoutBlob, TransferToHost2D, VIRTIO_GPU_EVENT_DISPLAY,
    },
};

/// Control queue index (the device also has a cursor queue, which this driver
/// does not use).
const CONTROL_QUEUE: u16 = 0;

/// Receive buffer for control responses of blocking commands.
const RECV_BUF_SIZE: usize = PAGE_SIZE;

/// Transmit buffer for control requests of blocking commands.
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
///
/// # Submission model
///
/// The control queue runs in the Linux virtio_gpu style (see [`crate::ctrl`]):
/// the async commands enqueue and return immediately without kicking, and
/// report device-side errors via the log rather than their return value.
/// Delivery happens when the consumer calls [`VirtIoGpu::ctrl_notify`] at its
/// transaction boundary (Linux `virtio_gpu_notify()`); a transaction that
/// never calls it leaves its commands undelivered. The blocking commands
/// (`GET_DISPLAY_INFO`, `GET_CAPSET_INFO`, `GET_CAPSET`) keep their historical
/// semantics through [`ControlQueue::request_sync`]: they wait for the
/// device's answer and can be freely mixed with async ones (the used ring is
/// FIFO, so ordering is preserved end to end).
pub struct VirtIoGpu<H: Hal, T: Transport> {
    transport: T,
    /// Rectangle of the current framebuffer, if one was set up.
    rect: Option<Rect>,
    /// DMA region backing the default framebuffer.
    frame_buffer_dma: Option<Dma<H>>,
    /// Queue carrying control commands, with the async submission state.
    pub(crate) ctrl: ControlQueue<H>,
    /// Transmit buffer for control requests of blocking commands.
    ///
    /// Blocking requests are copied here before they are submitted: the
    /// device-visible address is whatever [`Hal::share`] derives from it, and
    /// a caller's request may live anywhere — including a task kernel stack
    /// in a vmap'd region, which a linear-offset `virt_to_phys` translation
    /// does not map correctly. Allocator-owned buffers always resolve
    /// faithfully. (The async path does not need this: [`ControlQueue::enqueue`]
    /// copies commands into its own heap arenas before adding them.)
    queue_buf_send: Box<[u8]>,
    /// Receive buffer for control responses of blocking commands.
    queue_buf_recv: Box<[u8]>,
    /// Monotonic fence counter, shared by the blocking commands (response
    /// validation) and every fenced async submit.
    next_fence: u64,
    /// Whether the VIRGL 3D feature was negotiated.
    has_virgl: bool,
    /// Whether `VIRTIO_GPU_F_RESOURCE_BLOB` was negotiated.
    has_resource_blob: bool,
    /// Whether `VIRTIO_GPU_F_CONTEXT_INIT` was negotiated.
    has_context_init: bool,
    /// Device-advertised scanouts, capped to the protocol's sixteen entries.
    num_scanouts: u32,
    reset_done: bool,
}

impl<H: Hal, T: Transport> VirtIoGpu<H, T> {
    /// Initialises the device over `transport` and negotiates the feature set.
    ///
    /// `clock` must return monotonically increasing nanoseconds (for example
    /// a platform monotonic-time reader). It bounds the driver's blocking
    /// waits — `wait_fence` and the teardown drains — so a stalled or dead
    /// host unwedges the caller with [`Error::TimedOut`] after a fixed time
    /// instead of spinning forever under the consumer's global lock.
    pub fn new(mut transport: T, clock: fn() -> u64) -> Result<Self, Error> {
        let negotiated = transport.begin_init(SUPPORTED_FEATURES);

        let events_read = read_config!(transport, Config, events_read)?;
        let num_scanouts = read_config!(transport, Config, num_scanouts)?;
        log::debug!(
            "virtio-gpu config: events_read={events_read:#x}, num_scanouts={num_scanouts:#x}"
        );

        let ctrl = ControlQueue::new(
            &mut transport,
            CONTROL_QUEUE,
            negotiated.contains(Features::RING_INDIRECT_DESC),
            negotiated.contains(Features::RING_EVENT_IDX),
            clock,
        )?;

        let queue_buf_send = alloc::vec![0u8; SEND_BUF_SIZE].into_boxed_slice();
        let queue_buf_recv = alloc::vec![0u8; RECV_BUF_SIZE].into_boxed_slice();

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
            ctrl,
            queue_buf_send,
            queue_buf_recv,
            next_fence: 1,
            has_virgl,
            has_resource_blob,
            has_context_init,
            num_scanouts: num_scanouts.min(16),
            reset_done: false,
        })
    }

    /// Number of output slots exposed by this device.
    pub fn output_count(&self) -> u32 {
        self.num_scanouts
    }

    /// Stops all host DMA before an adapter releases backing after an
    /// ambiguous transport failure. The device must not be used again.
    ///
    /// The control queue's in-flight bookkeeping is discarded synchronously:
    /// nothing behind a device reset can still complete, so every further
    /// enqueue, notify, pump or wait fails fast with [`Error::DeviceLost`].
    pub fn reset(&mut self) {
        if self.reset_done {
            return;
        }
        // Drop the queue bookkeeping first: with the flag set, no concurrent
        // path can enqueue or kick between the two steps below.
        self.ctrl.invalidate();
        self.transport.set_status(DeviceStatus::empty());
        // VirtIO requires reading status 0 before the driver may release
        // queue memory or any backing the device could still access.
        while !self.transport.get_status().is_empty() {
            core::hint::spin_loop();
        }
        self.transport.queue_unset(CONTROL_QUEUE);
        self.reset_done = true;
    }

    #[cfg(feature = "rdif")]
    pub(crate) fn is_reset(&self) -> bool {
        self.reset_done
    }

    /// Reads the current connection and preferred rectangle for an output.
    pub fn output_info(&mut self, index: u32) -> Result<OutputInfo, Error> {
        if index >= self.num_scanouts {
            return Err(Error::InvalidParam);
        }
        let info = self.display_info()?;
        let mode = info.pmodes[index as usize];
        Ok(OutputInfo {
            rect: mode.rect,
            enabled: mode.enabled != 0,
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
        let info = self.output_info(0)?;
        Ok((info.rect.width, info.rect.height))
    }

    /// Sets up a framebuffer at the device's preferred resolution.
    ///
    /// See [`VirtIoGpu::change_resolution`] for the validity of the returned
    /// slice.
    pub fn setup_framebuffer(&mut self) -> Result<&mut [u8], Error> {
        let info = self.output_info(0)?;
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
        self.resource_create_2d(
            FRAMEBUFFER_RESOURCE_ID,
            width,
            height,
            Resource2dFormat::B8G8R8X8Unorm,
        )?;

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
            // attach does not leak a host resource. No completion observation
            // is needed before the DMA drops here: the attach failed, so the
            // device never received the range and cannot DMA into it — the
            // unref only frees the empty host-side resource object.
            let _ = self.resource_unref(FRAMEBUFFER_RESOURCE_ID);
            return Err(err);
        }

        // Bind the resource to the scanout. If that fails we must stop the
        // device from using the backing before freeing it: detach first, then
        // unref. `detached` must mean the host has actually finished the
        // teardown: `resource_unref` only enqueues the unref (fire-and-forget
        // with a fence), so this cold rollback path drains explicitly — the
        // FIFO pop covers the detach and the unref alike. On
        // TimedOut/QueueBroken the DMA is kept alive here and released by a
        // later retry or the device reset in `Drop`.
        if let Err(err) = self.set_scanout(rect, SCANOUT_ID, FRAMEBUFFER_RESOURCE_ID) {
            let detached = self
                .resource_detach_backing(FRAMEBUFFER_RESOURCE_ID)
                .and_then(|()| self.resource_unref(FRAMEBUFFER_RESOURCE_ID))
                .and_then(|_fence| self.wait_idle())
                .is_ok();
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
        // The device must be provably done with the backing before it is
        // handed back to the allocator: `resource_unref` only enqueues the
        // unref (fire-and-forget with a fence), so this cold teardown path
        // drains explicitly — the FIFO pop covers the stop-scanout, the
        // detach and the unref alike. On failure the `?` keeps the DMA in
        // `self.frame_buffer_dma` for a later retry or the reset in `Drop`.
        self.wait_idle()?;
        self.frame_buffer_dma = None;
        Ok(())
    }

    /// Bind the driver's own 2D framebuffer after another scanout was in use.
    pub fn restore_framebuffer_scanout(&mut self) -> Result<(), Error> {
        let rect = self.rect.ok_or(Error::NotReady)?;
        self.set_scanout(rect, SCANOUT_ID, FRAMEBUFFER_RESOURCE_ID)
    }

    /// Transfers the framebuffer to the host and flushes it to the scanout.
    ///
    /// Fire-and-forget: both commands are enqueued and delivered with one
    /// boundary notify; the console refresh path does not need completion.
    pub fn flush(&mut self) -> Result<(), Error> {
        let rect = self.rect.ok_or(Error::NotReady)?;
        self.transfer_to_host_2d(rect, 0, FRAMEBUFFER_RESOURCE_ID)?;
        self.resource_flush(rect, FRAMEBUFFER_RESOURCE_ID)?;
        self.ctrl_notify();
        Ok(())
    }

    // --- 2D resource and scanout commands ---

    /// Creates a 2D resource in the requested device format. Fire-and-forget
    /// (Linux `virtio_gpu_cmd_resource_create_2d` doesn't wait); see the
    /// [`VirtIoGpu`] submission-model docs. Device errors are logged, not
    /// returned.
    pub fn resource_create_2d(
        &mut self,
        resource_id: u32,
        width: u32,
        height: u32,
        format: Resource2dFormat,
    ) -> Result<(), Error> {
        self.ctrl
            .enqueue(
                &mut self.transport,
                &ResourceCreate2D {
                    header: CtrlHeader::with_type(Command::RESOURCE_CREATE_2D),
                    resource_id,
                    format,
                    width,
                    height,
                },
                None,
                0,
            )
            .map(|_| ())
    }

    /// Binds `resource_id` to `scanout_id` for the given display area.
    /// Fire-and-forget (Linux `virtio_gpu_primary_plane_update` doesn't wait);
    /// see the [`VirtIoGpu`] submission-model docs. Device errors are logged,
    /// not returned.
    pub fn set_scanout(
        &mut self,
        rect: Rect,
        scanout_id: u32,
        resource_id: u32,
    ) -> Result<(), Error> {
        self.ctrl
            .enqueue(
                &mut self.transport,
                &SetScanout {
                    header: CtrlHeader::with_type(Command::SET_SCANOUT),
                    rect,
                    scanout_id,
                    resource_id,
                },
                None,
                0,
            )
            .map(|_| ())
    }

    /// Binds a blob resource to a scanout with one packed pixel plane.
    /// Fire-and-forget, like the other scanout commands. Device errors are
    /// logged, not returned.
    pub fn set_scanout_blob(
        &mut self,
        rect: Rect,
        scanout_id: u32,
        resource_id: u32,
        format: u32,
        stride: u32,
        offset: u32,
    ) -> Result<(), Error> {
        if !self.has_resource_blob {
            return Err(Error::Unsupported);
        }
        self.ctrl
            .enqueue(
                &mut self.transport,
                &SetScanoutBlob {
                    header: CtrlHeader::with_type(Command::SET_SCANOUT_BLOB),
                    rect,
                    scanout_id,
                    resource_id,
                    width: rect.width,
                    height: rect.height,
                    format,
                    _padding: 0,
                    strides: [stride, 0, 0, 0],
                    offsets: [offset, 0, 0, 0],
                },
                None,
                0,
            )
            .map(|_| ())
    }

    /// Refreshes `rect` of `resource_id` on the display. Fire-and-forget
    /// (Linux `virtio_gpu_cmd_resource_flush` doesn't wait). Device errors are
    /// logged by the completion pump, not returned.
    pub fn resource_flush(&mut self, rect: Rect, resource_id: u32) -> Result<(), Error> {
        self.ctrl
            .enqueue(
                &mut self.transport,
                &ResourceFlush {
                    header: CtrlHeader::with_type(Command::RESOURCE_FLUSH),
                    rect,
                    resource_id,
                    _padding: 0,
                },
                None,
                0,
            )
            .map(|_| ())
    }

    /// Transfers `rect` of a 2D resource from guest memory to the host.
    /// Fire-and-forget; see the [`VirtIoGpu`] submission-model docs for the
    /// ordering argument. Device errors are logged, not returned.
    pub fn transfer_to_host_2d(
        &mut self,
        rect: Rect,
        offset: u64,
        resource_id: u32,
    ) -> Result<(), Error> {
        self.ctrl
            .enqueue(
                &mut self.transport,
                &TransferToHost2D {
                    header: CtrlHeader::with_type(Command::TRANSFER_TO_HOST_2D),
                    rect,
                    offset,
                    resource_id,
                    _padding: 0,
                },
                None,
                0,
            )
            .map(|_| ())
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
        // SAFETY: the caller promises this one range remains valid until
        // detach or unref, exactly as required by the multi-entry method.
        unsafe {
            self.resource_attach_backing_segments(resource_id, &[BlobMemory { paddr, length }])
        }
    }

    /// Attaches device-visible ranges to a resource in their supplied order.
    /// Fire-and-forget; the entries ride the command as its data buffer.
    ///
    /// # Safety
    ///
    /// Every range must remain mapped, allocated and free of conflicting CPU
    /// access until the device confirms detach or unref. The caller must own
    /// all ranges for the entire attachment lifetime.
    pub unsafe fn resource_attach_backing_segments(
        &mut self,
        resource_id: u32,
        segments: &[BlobMemory],
    ) -> Result<(), Error> {
        if segments.is_empty() {
            return Err(Error::InvalidParam);
        }
        let nr_entries = u32::try_from(segments.len()).map_err(|_| Error::Overflow)?;
        let capacity = segments
            .len()
            .checked_mul(size_of::<MemEntry>())
            .ok_or(Error::Overflow)?;
        let mut data = Vec::with_capacity(capacity);
        for segment in segments {
            if segment.length == 0 {
                return Err(Error::InvalidParam);
            }
            segment
                .paddr
                .checked_add(u64::from(segment.length))
                .ok_or(Error::Overflow)?;
            data.extend_from_slice(
                MemEntry {
                    addr: segment.paddr,
                    length: segment.length,
                    padding: 0,
                }
                .as_bytes(),
            );
        }
        self.ctrl
            .enqueue(
                &mut self.transport,
                &ResourceAttachBacking {
                    header: CtrlHeader::with_type(Command::RESOURCE_ATTACH_BACKING),
                    resource_id,
                    nr_entries,
                },
                Some(&data),
                0,
            )
            .map(|_| ())
    }

    /// Detaches the backing memory from a resource.
    ///
    /// After the host processes this command it no longer reads or writes the
    /// ranges that were attached. Fire-and-forget: the caller is responsible
    /// for the completion proof — drain ([`VirtIoGpu::wait_idle`]) or observe
    /// a later fenced command's fence — before releasing the memory. The
    /// driver's teardown paths pair this command with the fenced
    /// `resource_unref` and drain explicitly; the OS layer waits on the
    /// unref's fence.
    pub fn resource_detach_backing(&mut self, resource_id: u32) -> Result<(), Error> {
        self.ctrl
            .enqueue(
                &mut self.transport,
                &ResourceDetachBacking {
                    header: CtrlHeader::with_type(Command::RESOURCE_DETACH_BACKING),
                    resource_id,
                    _padding: 0,
                },
                None,
                0,
            )
            .map(|_| ())
    }

    /// Releases a resource.
    ///
    /// The protocol has a single `RESOURCE_UNREF` for 2D and 3D resources, so
    /// this is also the only way to destroy a 3D resource. The command is
    /// submitted fire-and-forget with a fence and delivered; this returns the
    /// fence id as soon as it is on the ring. The fence's completion is the
    /// proof that the host stopped touching the released backing: the used
    /// ring is FIFO, so the pop of the fenced unref implies the pop of every
    /// earlier command (the same implicit ordering submits rely on). The
    /// guest memory previously attached to the resource may only be freed
    /// after the OS layer observes that fence outside the device lock.
    /// Cold recovery paths inside this crate that must not return without
    /// the proof drain explicitly via [`VirtIoGpu::wait_idle`].
    pub fn resource_unref(&mut self, resource_id: u32) -> Result<u64, Error> {
        let fence_id = self.alloc_fence()?;
        self.ctrl
            .enqueue(
                &mut self.transport,
                &ResourceUnref {
                    header: CtrlHeader::with_fence(Command::RESOURCE_UNREF, 0, fence_id),
                    resource_id,
                    _padding: 0,
                },
                None,
                fence_id,
            )
            .map(|_| ())?;
        self.ctrl_notify();
        Ok(fence_id)
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
    /// it is truncated to 64 bytes. Fire-and-forget (Linux
    /// `virtio_gpu_cmd_context_create` doesn't wait): every later command for
    /// this context is enqueued after it on the same ring. Device errors are
    /// logged, not returned.
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
        self.ctrl
            .enqueue(&mut self.transport, &cmd, None, 0)
            .map(|_| ())
    }

    /// Destroys a 3D rendering context. Fire-and-forget (Linux
    /// `virtio_gpu_cmd_context_destroy` doesn't wait). Device errors are
    /// logged, not returned.
    pub fn ctx_destroy(&mut self, ctx_id: u32) -> Result<(), Error> {
        self.require_virgl()?;
        self.ctrl
            .enqueue(
                &mut self.transport,
                &CtrlHeader::with_type_and_ctx(Command::CTX_DESTROY, ctx_id),
                None,
                0,
            )
            .map(|_| ())
    }

    /// Attaches a resource to a rendering context. Fire-and-forget (Linux
    /// `virtio_gpu_cmd_ctx_attach_resource` doesn't wait); see the
    /// [`VirtIoGpu`] submission-model docs for the ordering argument. Device
    /// errors are logged, not returned.
    pub fn ctx_attach_resource(&mut self, ctx_id: u32, resource_id: u32) -> Result<(), Error> {
        self.require_virgl()?;
        self.ctrl
            .enqueue(
                &mut self.transport,
                &CmdCtxResource {
                    header: CtrlHeader::with_type_and_ctx(Command::CTX_ATTACH_RESOURCE, ctx_id),
                    resource_id,
                    _padding: 0,
                },
                None,
                0,
            )
            .map(|_| ())
    }

    /// Detaches a resource from a rendering context. Fire-and-forget (Linux
    /// `virtio_gpu_cmd_ctx_detach_resource` doesn't wait). Device errors are
    /// logged, not returned.
    pub fn ctx_detach_resource(&mut self, ctx_id: u32, resource_id: u32) -> Result<(), Error> {
        self.require_virgl()?;
        self.ctrl
            .enqueue(
                &mut self.transport,
                &CmdCtxResource {
                    header: CtrlHeader::with_type_and_ctx(Command::CTX_DETACH_RESOURCE, ctx_id),
                    resource_id,
                    _padding: 0,
                },
                None,
                0,
            )
            .map(|_| ())
    }

    /// Creates a 3D resource such as a texture, render target or buffer.
    /// Fire-and-forget (Linux `virtio_gpu_cmd_resource_create_3d` doesn't
    /// wait); see the [`VirtIoGpu`] submission-model docs for the ordering
    /// argument. Device errors are logged, not returned.
    pub fn resource_create_3d(&mut self, params: ResourceCreate3d) -> Result<(), Error> {
        self.require_virgl()?;
        self.ctrl
            .enqueue(
                &mut self.transport,
                &CmdResourceCreate3D {
                    header: CtrlHeader::with_type_and_ctx(
                        Command::RESOURCE_CREATE_3D,
                        params.ctx_id,
                    ),
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
                },
                None,
                0,
            )
            .map(|_| ())
    }

    /// Transfers a 3D resource from guest memory to the host. Fire-and-forget
    /// (Linux `virtio_gpu_cmd_transfer_to_host_3d` doesn't wait): the host
    /// applies it in ring order, before anything enqueued later. Device errors
    /// are logged, not returned.
    pub fn transfer_to_host_3d(&mut self, params: Transfer3d) -> Result<(), Error> {
        self.require_virgl()?;
        self.ctrl
            .enqueue(
                &mut self.transport,
                &CmdTransferHost3D {
                    header: CtrlHeader::with_type_and_ctx(
                        Command::TRANSFER_TO_HOST_3D,
                        params.ctx_id,
                    ),
                    box_: params.box_,
                    offset: params.offset,
                    resource_id: params.resource_id,
                    level: params.level,
                    stride: params.stride,
                    layer_stride: params.layer_stride,
                },
                None,
                0,
            )
            .map(|_| ())
    }

    /// Transfers a 3D resource from the host to guest memory. The command is
    /// submitted fire-and-forget **with a fence** and returns the fence id:
    /// the fence's completion proves the host applied the transfer (the used
    /// ring is FIFO, so its pop implies the pop of every earlier command),
    /// which is what makes the guest memory valid to read. The OS layer
    /// observes that fence outside the device lock and syncs the backing for
    /// the CPU before letting its caller read back (Linux instead relies on
    /// dma_resv deferred destruction and returns without waiting).
    pub fn transfer_from_host_3d(&mut self, params: Transfer3d) -> Result<u64, Error> {
        self.require_virgl()?;
        let fence_id = self.alloc_fence()?;
        self.ctrl
            .enqueue(
                &mut self.transport,
                &CmdTransferHost3D {
                    header: CtrlHeader::with_fence(
                        Command::TRANSFER_FROM_HOST_3D,
                        params.ctx_id,
                        fence_id,
                    ),
                    box_: params.box_,
                    offset: params.offset,
                    resource_id: params.resource_id,
                    level: params.level,
                    stride: params.stride,
                    layer_stride: params.layer_stride,
                },
                None,
                fence_id,
            )
            .map(|_| ())?;
        self.ctrl_notify();
        Ok(fence_id)
    }

    /// Submits a virgl command stream to a rendering context and returns the
    /// fence id the host will signal.
    ///
    /// `cmds` is the encoded stream produced by the Mesa virgl Gallium driver
    /// in userspace and is sent as a second buffer next to the `SUBMIT_3D`
    /// header. The submit is fire-and-forget: it returns as soon as the stream
    /// is enqueued, not when rendering has finished. The command carries
    /// `VIRTIO_GPU_FLAG_FENCE` with the returned id, so the host pops the used
    /// entry — and thus advances the fence high-water mark past the id — only
    /// when the virgl fence fires, i.e. after the host finished decoding and
    /// executing the batch (Linux fences every EXECBUFFER; `virtio_gpu_init_submit`,
    /// virtgpu_submit.c). Block on [`VirtIoGpu::wait_fence`] (which also
    /// delivers the batch) or poll [`VirtIoGpu::fence_completed`] alongside
    /// [`VirtIoGpu::ctrl_notify`] before reading back anything the batch
    /// renders. Mirrors Linux `virtio_gpu_cmd_submit` (enqueue-and-return).
    pub fn submit_3d(&mut self, ctx_id: u32, cmds: &[u8]) -> Result<u64, Error> {
        self.require_virgl()?;
        if !cmds.len().is_multiple_of(size_of::<u32>()) {
            return Err(Error::InvalidParam);
        }
        let size = u32::try_from(cmds.len()).map_err(|_| Error::Overflow)?;
        let fence_id = self.alloc_fence()?;
        let req = CmdSubmit3D {
            header: CtrlHeader::with_fence(Command::SUBMIT_3D, ctx_id, fence_id),
            size,
            _padding: 0,
        };
        // Fire-and-forget: the popped response is dropped (the fence is the
        // completion signal), and the command stream is copied into a heap
        // box by the queue.
        self.ctrl
            .enqueue(&mut self.transport, &req, Some(cmds), fence_id)
            .map(|_| fence_id)
    }

    /// Creates a blob resource such as host-visible memory or a dma-buf.
    /// Fire-and-forget (Linux `virtio_gpu_cmd_resource_create_blob` doesn't
    /// wait). Device errors are logged, not returned.
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
    /// guest memory at the addresses in the entries. The caller must guarantee
    /// that every range is valid device-accessible memory and stays allocated
    /// and free of concurrent access for as long as the blob resource exists,
    /// that is until the matching [`VirtIoGpu::resource_unref`]. The ranges
    /// must also cover `size` bytes in total: a blob larger than its backing
    /// would let the device reach past the end of the provided ranges.
    /// `HOST3D` blobs must pass no entries at all.
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
                // The device reads and writes `paddr..paddr + length`; a
                // wrapped extent would name a range unrelated to the caller's
                // backing.
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

        self.ctrl
            .enqueue(
                &mut self.transport,
                &CmdResourceCreateBlob {
                    header: CtrlHeader::with_type_and_ctx(
                        Command::RESOURCE_CREATE_BLOB,
                        params.ctx_id,
                    ),
                    resource_id: params.resource_id,
                    blob_mem: params.blob_mem,
                    blob_flags: params.blob_flags,
                    nr_entries,
                    blob_id: params.blob_id,
                    size: params.size,
                },
                (!data.is_empty()).then_some(&data),
                0,
            )
            .map(|_| ())
    }

    // --- Completion and fence API ---

    /// Delivers all fire-and-forget control commands accumulated since the
    /// last kick with a single MMIO write — Linux `virtio_gpu_notify()` at the
    /// transaction boundary.
    ///
    /// This is what delivers every command enqueued since the last notify —
    /// a transaction that enqueues fire-and-forget commands and never calls
    /// this leaves them undelivered (the Linux DRM ioctls carry the same
    /// obligation, discharged by their end-of-ioctl `virtio_gpu_notify()`).
    /// No-op when nothing has accumulated.
    pub fn ctrl_notify(&mut self) {
        self.ctrl.notify(&mut self.transport);
    }

    /// Pop and reclaim every used control-queue entry currently available.
    ///
    /// Recycles the descriptors, advances the fence high-water mark, and logs
    /// device-side error responses. This is the counterpart of Linux's
    /// IRQ-driven `virtio_gpu_dequeue_ctrl_func`; call it from the task-context
    /// service path and/or from the polling wait paths. Entries belonging to
    /// an in-flight blocking command are left for their waiter.
    ///
    /// # Errors
    ///
    /// Returns [`Error::QueueBroken`] after the device was reset or the queue
    /// observed a foreign completion; the queue is then unusable.
    pub fn pump_completions(&mut self) -> Result<(), Error> {
        self.ctrl.pump_completions(&mut self.transport)
    }

    /// Block until the fence identified by `fence_id` (and everything enqueued
    /// before it) has been popped from the control queue. Implicit ordering:
    /// any entry completes all ≤ its id. Bounded by the wait timeout — see
    /// [`VirtIoGpu::new`].
    ///
    /// Also delivers fire-and-forget commands accumulated since the last kick
    /// before waiting — the fenced entry itself may not have reached the host
    /// yet (the consumer's boundary [`VirtIoGpu::ctrl_notify`] may not have
    /// run).
    pub fn wait_fence(&mut self, fence_id: u64) -> Result<(), Error> {
        self.ctrl.wait_fence(&mut self.transport, fence_id)
    }

    /// Block until every enqueued command — fire-and-forget and synchronous
    /// alike — has been popped from the queue, and everything parked has been
    /// re-added and popped too. Teardown paths use this as the completion
    /// proof before handing device-accessible memory back to its owner.
    ///
    /// Bounded by the wait timeout: on [`Error::TimedOut`] the host is
    /// unrecoverably stalled and memory released after this error may race a
    /// device that is still (or was never) done — callers that cannot accept
    /// that race must reset the device instead (see [`VirtIoGpu::reset`]).
    pub fn wait_idle(&mut self) -> Result<(), Error> {
        self.ctrl.wait_idle(&mut self.transport)
    }

    /// Non-blocking fence query: has `fence_id` (and everything enqueued
    /// before it) already been popped, i.e. has its virgl fence fired?
    ///
    /// Only reflects batches that have actually been delivered to the host; a
    /// consumer that only polls must ensure delivery itself (e.g.
    /// [`VirtIoGpu::ctrl_notify`] at the transaction boundary, or a
    /// [`VirtIoGpu::wait_fence`]). The high-water mark only advances when
    /// completed entries are popped, so without a service path calling
    /// [`VirtIoGpu::pump_completions`] the poll loop must call it itself. The
    /// counterpart of Linux `dma_resv_test_signaled` in the NOWAIT probe of
    /// `virtio_gpu_wait_ioctl` (virtgpu_ioctl.c).
    pub fn fence_completed(&self, fence_id: u64) -> bool {
        self.ctrl.fence_completed(fence_id)
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
    /// is submitted (see the field's documentation for why the copy is not
    /// optional), and only the copied bytes are handed to the queue: exposing
    /// the whole page would present the untouched tail as part of the command.
    fn request_with_len<Req, Rsp>(&mut self, req: Req) -> Result<(Rsp, usize), Error>
    where
        Req: IntoBytes + Immutable,
        Rsp: FromBytes,
    {
        let (req_len, fence_id) = self.prepare_request(&req)?;
        let used_len = match self.ctrl.request_sync(
            &mut self.transport,
            &[&self.queue_buf_send[..req_len]],
            &mut [&mut self.queue_buf_recv],
        ) {
            Ok(len) => len as usize,
            Err(_) => {
                // Submission may have reached the device. Stop DMA before
                // callers can drop backing after this ambiguous completion.
                self.reset();
                return Err(Error::DeviceLost);
            }
        };
        let response = self.finish_request(used_len, fence_id)?;
        Ok((response, used_len))
    }

    fn finish_request<Rsp: FromBytes>(
        &mut self,
        used_len: usize,
        fence_id: u64,
    ) -> Result<Rsp, Error> {
        let header = match self.check_fence_response(used_len, fence_id) {
            Ok(header) => header,
            Err(_) => {
                // A used descriptor alone does not prove that a control
                // command has stopped accessing its backing.
                self.reset();
                return Err(Error::DeviceLost);
            }
        };
        if let Some(error) = header.rejection() {
            return Err(error);
        }
        self.parse_response(used_len)
    }

    fn prepare_request<Req: IntoBytes + Immutable>(
        &mut self,
        req: &Req,
    ) -> Result<(usize, u64), Error> {
        if self.reset_done {
            return Err(Error::DeviceLost);
        }
        let req_len = copy_request_into(&mut self.queue_buf_send, req)?;
        if req_len < size_of::<CtrlHeader>() {
            return Err(Error::InvalidParam);
        }
        let fence_id = self.alloc_fence()?;
        // Every synchronous operation reports synchronous completion. VirtIO
        // GPU may otherwise return a used response before host processing
        // ends.
        self.queue_buf_send[4..8].copy_from_slice(&GPU_FLAG_FENCE.to_le_bytes());
        self.queue_buf_send[8..16].copy_from_slice(&fence_id.to_le_bytes());
        Ok((req_len, fence_id))
    }

    /// Reserves the next monotonic fence id, shared by the blocking commands
    /// and every fenced async submit so the two families can never alias.
    fn alloc_fence(&mut self) -> Result<u64, Error> {
        let fence_id = self.next_fence;
        self.next_fence = fence_id.checked_add(1).ok_or(Error::Overflow)?;
        Ok(fence_id)
    }

    fn check_fence_response(&self, used_len: usize, fence_id: u64) -> Result<CtrlHeader, Error> {
        if used_len > self.queue_buf_recv.len() {
            return Err(Error::ResponseTooLarge);
        }
        if used_len < size_of::<CtrlHeader>() {
            return Err(Error::InvalidResponse);
        }
        let (header, _) = CtrlHeader::read_from_prefix(&self.queue_buf_recv[..used_len])
            .map_err(|_| Error::InvalidResponse)?;
        header.check_fence(fence_id)?;
        Ok(header)
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
        // Confirm that the device stopped DMA before dropping queue memory
        // and any backing retained by the RDIF owner. The reset also
        // invalidates the control queue, so fire-and-forget commands still in
        // flight are dropped rather than waited for: a caller that hands
        // device-accessible memory to async commands must wait on the last
        // fence before releasing it (see [`VirtIoGpu::wait_fence`]).
        self.reset();
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

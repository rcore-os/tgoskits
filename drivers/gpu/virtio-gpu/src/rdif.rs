//! OS-independent GPU and display capabilities over one VirtIO transport.

use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet, VecDeque},
    format,
    string::ToString,
    sync::Arc,
    vec::Vec,
};
use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use rdif_display::{
    DisplayController, DisplayError, DisplayEvent, DisplayState, Mode, OutputId,
    OutputInfo as DisplayOutputInfo, OutputKind, ScanoutBuffer,
};
use rdif_gpu::{
    Backing, BlobDescriptor, BufferDescriptor, BufferHandle, BusIdentity, CapsetInfo, Completion,
    CompletionStatus, ContextHandle, DmaDomainId, DriverVersion, GpuCapabilities, GpuDevice,
    GpuError, GpuIdentity, GpuIrqEndpoint, PixelFormat, Resource3d, Transfer3d, VirglOps,
};
use virtio_drivers::{Hal, transport::Transport};

use crate::{BlobMemory, Error, GpuBox, Rect, ResourceCreate3d, ResourceCreateBlob, VirtIoGpu};

struct Resource {
    id: u32,
    backing: Option<Arc<dyn Backing>>,
    kind: ResourceKind,
    attached: bool,
}

#[derive(Clone, Copy)]
enum ResourceKind {
    TwoD(BufferDescriptor),
    ThreeD { width: u32, height: u32 },
    Blob { size: u64 },
}

// Tokens are never reused, including across device instances. Failed
// creations consume a token so stale handles cannot alias later resources.
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn new_token() -> Result<NonZeroU64, GpuError> {
    let id = NEXT_HANDLE
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| GpuError::OutOfMemory)?;
    NonZeroU64::new(id).ok_or(GpuError::OutOfMemory)
}

/// One VirtIO GPU with a single resource table and display owner.
///
/// The adapter is independent of any OS allocator or lock. The caller supplies
/// device-domain DMA backing and serializes mutable access. The transport and
/// backing remain owned until the device confirms detach/unref or is reset.
pub struct VirtIoGpuDevice<H: Hal, T: Transport> {
    raw: VirtIoGpu<H, T>,
    identity: GpuIdentity,
    dma_domain: DmaDomainId,
    next_resource: u32,
    next_context: u32,
    next_fence: u64,
    resources: BTreeMap<u64, Resource>,
    contexts: BTreeMap<u64, u32>,
    attachments: BTreeSet<(u64, u64)>,
    outputs: Vec<DisplayOutputInfo>,
    states: Vec<Option<DisplayState>>,
    events: VecDeque<DisplayEvent>,
    irq_endpoint: Option<Box<dyn GpuIrqEndpoint>>,
    lost: bool,
}

// SAFETY: all protocol and resource operations require &mut self, so moving
// this owner between CPUs cannot introduce concurrent transport or DMA access.
// The independently shared IRQ endpoint never borrows fields of this object;
// its transport bridge must serialize raw transport access separately.
unsafe impl<H: Hal, T: Transport + Send> Send for VirtIoGpuDevice<H, T> {}

impl<H: Hal, T: Transport> VirtIoGpuDevice<H, T> {
    /// Builds the RDIF owner from an initialized VirtIO protocol device.
    pub fn new(
        mut raw: VirtIoGpu<H, T>,
        dma_domain: DmaDomainId,
        mut identity: GpuIdentity,
        irq_endpoint: Option<Box<dyn GpuIrqEndpoint>>,
    ) -> Result<Self, Error> {
        let mut outputs = Vec::new();
        for index in 0..raw.output_count() {
            outputs.push(read_output(&mut raw, index)?);
        }
        identity.driver_name = "virtio_gpu".to_string();
        if identity.device_name.is_empty() {
            identity.device_name = "virtio-gpu0".to_string();
        }
        if identity.description.is_empty() {
            identity.description = "VirtIO GPU".to_string();
        }
        let states = alloc::vec![None; outputs.len()];
        Ok(Self {
            raw,
            identity,
            dma_domain,
            next_resource: 1,
            next_context: 1,
            next_fence: 1,
            resources: BTreeMap::new(),
            contexts: BTreeMap::new(),
            attachments: BTreeSet::new(),
            outputs,
            states,
            events: VecDeque::new(),
            irq_endpoint,
            lost: false,
        })
    }

    /// Creates an identity for a transport with no discoverable bus metadata.
    pub fn virtual_identity() -> GpuIdentity {
        GpuIdentity {
            driver_name: "virtio_gpu".to_string(),
            device_name: "virtio-gpu0".to_string(),
            driver_version: DriverVersion {
                major: 0,
                minor: 1,
                patch: 0,
            },
            description: "VirtIO GPU".to_string(),
            bus: BusIdentity::Virtual,
            modalias: None,
        }
    }

    fn ensure_ready(&self) -> Result<(), GpuError> {
        if self.lost {
            Err(GpuError::DeviceLost)
        } else {
            Ok(())
        }
    }

    fn alloc_resource(&mut self) -> Result<(u32, BufferHandle), GpuError> {
        let id = self.next_resource;
        self.next_resource = id.checked_add(1).ok_or(GpuError::OutOfMemory)?;
        let handle = BufferHandle::new(new_token()?);
        Ok((id, handle))
    }

    fn alloc_context(&mut self) -> Result<(u32, ContextHandle), GpuError> {
        let id = self.next_context;
        self.next_context = id.checked_add(1).ok_or(GpuError::OutOfMemory)?;
        let handle = ContextHandle::new(new_token()?);
        Ok((id, handle))
    }

    fn resource_id(&self, handle: BufferHandle) -> Result<u32, GpuError> {
        self.resources
            .get(&handle.id().get())
            .map(|resource| resource.id)
            .ok_or(GpuError::InvalidHandle)
    }

    fn context_id(&self, handle: ContextHandle) -> Result<u32, GpuError> {
        self.contexts
            .get(&handle.id().get())
            .copied()
            .ok_or(GpuError::InvalidHandle)
    }

    fn validate_backing(
        &self,
        backing: &dyn Backing,
        size: usize,
    ) -> Result<Vec<BlobMemory>, GpuError> {
        if size == 0 || backing.len() < size || backing.domain_id() != self.dma_domain {
            return Err(GpuError::InvalidArgument);
        }
        let mut remaining = size;
        let mut entries = Vec::new();
        for segment in backing.segments() {
            if remaining == 0 {
                break;
            }
            let mut address = segment.addr.as_u64();
            let mut length = segment.len.get().min(remaining);
            while length > 0 {
                let part = length.min(u32::MAX as usize);
                let part_u32 = u32::try_from(part).map_err(|_| GpuError::InvalidArgument)?;
                address
                    .checked_add(u64::from(part_u32))
                    .ok_or(GpuError::InvalidArgument)?;
                entries.push(BlobMemory {
                    paddr: address,
                    length: part_u32,
                });
                address += u64::from(part_u32);
                length -= part;
                remaining -= part;
            }
        }
        if remaining != 0 {
            return Err(GpuError::InvalidArgument);
        }
        Ok(entries)
    }

    fn attach_backing(
        &mut self,
        id: u32,
        backing: &Arc<dyn Backing>,
        size: usize,
    ) -> Result<(), GpuError> {
        let entries = self.validate_backing(backing.as_ref(), size)?;
        backing.sync_for_device(0..size)?;
        // SAFETY: backing is retained in the resource table on success; on
        // failure the caller unreferences or resets the device before release.
        unsafe { self.raw.resource_attach_backing_segments(id, &entries) }.map_err(map_error)
    }

    fn cleanup_failed_create(&mut self, id: u32) {
        // A failed attach may have reached the host. Confirmation through
        // UNREF permits backing release; otherwise reset stops all DMA.
        if self.raw.resource_unref(id).is_err() {
            self.mark_lost();
        }
    }

    fn mark_lost(&mut self) {
        // Reset stops all DMA before any backing or scanout reference drops.
        self.raw.reset();
        self.lost = true;
        self.states.fill(None);
        self.attachments.clear();
        self.contexts.clear();
        self.resources.clear();
    }

    fn output_index(&self, id: OutputId) -> Result<usize, DisplayError> {
        let index = id.id() as usize;
        if index < self.outputs.len() {
            Ok(index)
        } else {
            Err(DisplayError::InvalidOutput)
        }
    }

    fn scanout_resource(&self, state: &DisplayState) -> Result<Option<u32>, DisplayError> {
        let Some(framebuffer) = &state.framebuffer else {
            return Ok(None);
        };
        let ScanoutBuffer::Gpu(handle) = &framebuffer.buffer else {
            return Err(DisplayError::Unsupported);
        };
        let resource = self
            .resources
            .get(&handle.id().get())
            .ok_or(DisplayError::Gpu(GpuError::InvalidHandle))?;
        let expected = BufferDescriptor::Image2d {
            width: framebuffer.width,
            height: framebuffer.height,
            stride: framebuffer.stride,
            format: framebuffer.format,
        };
        match resource.kind {
            ResourceKind::TwoD(descriptor) if descriptor == expected => {}
            ResourceKind::ThreeD { width, height }
                if width >= framebuffer.width && height >= framebuffer.height => {}
            ResourceKind::Blob { size }
                if u64::try_from(
                    framebuffer
                        .required_len()
                        .ok_or(DisplayError::InvalidState)?,
                )
                .is_ok_and(|needed| needed <= size) => {}
            _ => return Err(DisplayError::InvalidState),
        }
        Ok(Some(resource.id))
    }

    fn rollback_scanout(&mut self, index: usize) -> Result<(), DisplayError> {
        let old = self.states[index]
            .as_ref()
            .and_then(|state| state.framebuffer.clone());
        if let Some(framebuffer) = old {
            let ScanoutBuffer::Gpu(handle) = &framebuffer.buffer else {
                return Err(DisplayError::InvalidState);
            };
            let rect = Rect {
                x: 0,
                y: 0,
                width: framebuffer.width,
                height: framebuffer.height,
            };
            let resource_id = self.resource_id(*handle).map_err(DisplayError::Gpu)?;
            self.bind_scanout(index as u32, resource_id, &framebuffer, rect)
        } else {
            self.raw
                .set_scanout(Rect::default(), index as u32, 0)
                .map_err(map_display_error)
        }
    }

    fn fail_commit_after_bind(&mut self, index: usize, error: DisplayError) -> DisplayError {
        if self.rollback_scanout(index).is_err() {
            self.mark_lost();
            DisplayError::DeviceLost
        } else {
            error
        }
    }

    fn bind_scanout(
        &mut self,
        output: u32,
        resource_id: u32,
        framebuffer: &rdif_display::Framebuffer,
        rect: Rect,
    ) -> Result<(), DisplayError> {
        let kind = self
            .resources
            .values()
            .find(|resource| resource.id == resource_id)
            .ok_or(DisplayError::Gpu(GpuError::InvalidHandle))?
            .kind;
        match kind {
            ResourceKind::Blob { .. } => self
                .raw
                .set_scanout_blob(
                    rect,
                    output,
                    resource_id,
                    match framebuffer.format {
                        PixelFormat::Xrgb8888 => 2,
                        PixelFormat::Argb8888 => 1,
                        _ => return Err(DisplayError::Unsupported),
                    },
                    framebuffer.stride,
                    u32::try_from(framebuffer.offset).map_err(|_| DisplayError::InvalidState)?,
                )
                .map_err(map_display_error),
            ResourceKind::TwoD(_) | ResourceKind::ThreeD { .. } => self
                .raw
                .set_scanout(rect, output, resource_id)
                .map_err(map_display_error),
        }
    }
}

impl<H: Hal + 'static, T: Transport + Send + 'static> DisplayController for VirtIoGpuDevice<H, T> {
    fn output_count(&self) -> u32 {
        self.outputs.len() as u32
    }

    fn output(&self, id: OutputId) -> Result<DisplayOutputInfo, DisplayError> {
        Ok(self.outputs[self.output_index(id)?].clone())
    }

    fn current_state(&self, id: OutputId) -> Result<Option<DisplayState>, DisplayError> {
        Ok(self.states[self.output_index(id)?].clone())
    }

    fn check(&self, state: &DisplayState) -> Result<(), DisplayError> {
        if self.lost {
            return Err(DisplayError::DeviceLost);
        }
        let index = self.output_index(state.output)?;
        if state.framebuffer.is_none() {
            return if state.mode.is_none() && state.damage.is_empty() {
                Ok(())
            } else {
                Err(DisplayError::InvalidState)
            };
        }
        let framebuffer = state.framebuffer.as_ref().unwrap();
        let mode = state.mode.ok_or(DisplayError::InvalidState)?;
        if !self.outputs[index].connected
            || !self.outputs[index].modes.contains(&mode)
            || mode.width != framebuffer.width
            || mode.height != framebuffer.height
            || !matches!(
                framebuffer.format,
                PixelFormat::Xrgb8888 | PixelFormat::Argb8888
            )
            || framebuffer.required_len().is_none()
        {
            return Err(DisplayError::InvalidState);
        }
        self.scanout_resource(state)?;
        let ScanoutBuffer::Gpu(handle) = &framebuffer.buffer else {
            return Err(DisplayError::Unsupported);
        };
        if matches!(
            self.resources[&handle.id().get()].kind,
            ResourceKind::TwoD(_) | ResourceKind::ThreeD { .. }
        ) && (framebuffer.offset != 0
            || framebuffer.stride
                != framebuffer
                    .width
                    .checked_mul(4)
                    .ok_or(DisplayError::InvalidState)?)
        {
            return Err(DisplayError::InvalidState);
        }
        for damage in &state.damage {
            if damage.width == 0
                || damage.height == 0
                || damage
                    .x
                    .checked_add(damage.width)
                    .is_none_or(|end| end > mode.width)
                || damage
                    .y
                    .checked_add(damage.height)
                    .is_none_or(|end| end > mode.height)
            {
                return Err(DisplayError::InvalidState);
            }
        }
        Ok(())
    }

    fn commit(&mut self, state: &DisplayState) -> Result<Completion, DisplayError> {
        self.check(state)?;
        let index = self.output_index(state.output)?;
        let Some(framebuffer) = &state.framebuffer else {
            if let Err(error) = self.raw.set_scanout(Rect::default(), index as u32, 0) {
                return Err(self.fail_commit_after_bind(index, map_display_error(error)));
            }
            self.states[index] = Some(state.clone());
            return Ok(Completion::Complete);
        };
        let resource_id = self
            .scanout_resource(state)?
            .ok_or(DisplayError::InvalidState)?;
        let ScanoutBuffer::Gpu(handle) = &framebuffer.buffer else {
            return Err(DisplayError::Unsupported);
        };
        let kind = self.resources[&handle.id().get()].kind;
        let full = Rect {
            x: 0,
            y: 0,
            width: framebuffer.width,
            height: framebuffer.height,
        };
        // Only guest-backed 2D resources need a host transfer. Virgl 3D and
        // blob resources are already populated by their rendering commands.
        if matches!(kind, ResourceKind::TwoD(_)) {
            let required = framebuffer
                .required_len()
                .ok_or(DisplayError::InvalidState)?;
            if let Some(backing) = &self.resources[&handle.id().get()].backing {
                backing
                    .sync_for_device(0..required)
                    .map_err(DisplayError::Gpu)?;
            }
            self.raw
                .transfer_to_host_2d(full, 0, resource_id)
                .map_err(map_display_error)?;
        }
        if let Err(error) = self.bind_scanout(index as u32, resource_id, framebuffer, full) {
            return Err(self.fail_commit_after_bind(index, error));
        }
        if let Err(error) = self.raw.resource_flush(full, resource_id) {
            return Err(self.fail_commit_after_bind(index, map_display_error(error)));
        }
        self.states[index] = Some(state.clone());
        self.events.push_back(DisplayEvent::CommitCompleted {
            output: state.output,
            completion: Completion::Complete,
        });
        Ok(Completion::Complete)
    }

    fn commit_status(&mut self, completion: Completion) -> Result<CompletionStatus, DisplayError> {
        <Self as GpuDevice>::completion_status(self, completion).map_err(DisplayError::Gpu)
    }

    fn poll_event(&mut self) -> Option<DisplayEvent> {
        self.events.pop_front()
    }
}

fn read_output<H: Hal, T: Transport>(
    raw: &mut VirtIoGpu<H, T>,
    index: u32,
) -> Result<DisplayOutputInfo, Error> {
    let current = raw.output_info(index)?;
    let mode =
        (current.enabled && current.rect.width > 0 && current.rect.height > 0).then_some(Mode {
            width: current.rect.width,
            height: current.rect.height,
            refresh_millihz: 0,
        });
    Ok(DisplayOutputInfo {
        id: OutputId::new(index),
        name: format!("Virtual-{}", index + 1),
        kind: OutputKind::Virtual,
        connected: current.enabled,
        physical_size_mm: None,
        modes: mode.into_iter().collect(),
        preferred_mode: mode,
        formats: alloc::vec![PixelFormat::Xrgb8888, PixelFormat::Argb8888],
    })
}

fn map_error(error: Error) -> GpuError {
    match error {
        Error::Unsupported => GpuError::Unsupported,
        Error::NotReady => GpuError::NotReady,
        Error::InvalidParam | Error::Overflow => GpuError::InvalidArgument,
        Error::DmaError => GpuError::OutOfMemory,
        Error::VirtIo(virtio_drivers::Error::DmaError) => GpuError::OutOfMemory,
        Error::VirtIo(virtio_drivers::Error::QueueFull | virtio_drivers::Error::AlreadyUsed) => {
            GpuError::Busy
        }
        _ => GpuError::Io,
    }
}

fn map_display_error(error: Error) -> DisplayError {
    DisplayError::Gpu(map_error(error))
}

impl<H: Hal + 'static, T: Transport + Send + 'static> rdif_gpu::DriverGeneric
    for VirtIoGpuDevice<H, T>
{
    fn name(&self) -> &str {
        &self.identity.device_name
    }
}

impl<H: Hal + 'static, T: Transport + Send + 'static> GpuDevice for VirtIoGpuDevice<H, T> {
    fn identity(&self) -> GpuIdentity {
        self.identity.clone()
    }

    fn capabilities(&self) -> GpuCapabilities {
        GpuCapabilities {
            dma_domain: self.dma_domain,
            supports_image_2d: true,
            supports_3d: self.raw.has_virgl(),
            supports_blob: self.raw.has_resource_blob(),
            supports_context_init: self.raw.has_context_init(),
        }
    }

    fn create_buffer(
        &mut self,
        desc: BufferDescriptor,
        backing: Arc<dyn Backing>,
    ) -> Result<BufferHandle, GpuError> {
        self.ensure_ready()?;
        let BufferDescriptor::Image2d {
            width,
            height,
            stride,
            format,
        } = desc
        else {
            return Err(GpuError::Unsupported);
        };
        let packed_stride = width.checked_mul(4).ok_or(GpuError::InvalidArgument)?;
        if stride != packed_stride
            || !matches!(format, PixelFormat::Xrgb8888 | PixelFormat::Argb8888)
        {
            return Err(GpuError::Unsupported);
        }
        let size = desc.required_len().ok_or(GpuError::InvalidArgument)?;
        self.validate_backing(backing.as_ref(), size)?;
        let (id, handle) = self.alloc_resource()?;
        if let Err(error) = self.raw.resource_create_2d(id, width, height) {
            // A transport failure may arrive after the host accepted CREATE.
            // UNREF confirmation (or reset) closes that ambiguous lifetime.
            self.cleanup_failed_create(id);
            return Err(map_error(error));
        }
        if let Err(error) = self.attach_backing(id, &backing, size) {
            self.cleanup_failed_create(id);
            return Err(error);
        }
        self.resources.insert(
            handle.id().get(),
            Resource {
                id,
                backing: Some(backing),
                kind: ResourceKind::TwoD(desc),
                attached: true,
            },
        );
        Ok(handle)
    }

    fn buffer_backing(&self, buffer: BufferHandle) -> Result<Option<Arc<dyn Backing>>, GpuError> {
        self.resources
            .get(&buffer.id().get())
            .map(|resource| resource.backing.clone())
            .ok_or(GpuError::InvalidHandle)
    }

    fn release_buffer(&mut self, buffer: BufferHandle) -> Result<(), GpuError> {
        self.ensure_ready()?;
        let key = buffer.id().get();
        let id = self.resource_id(buffer)?;
        if self
            .attachments
            .iter()
            .any(|(_, resource)| *resource == key)
            || self.states.iter().flatten().any(|state| {
                state.framebuffer.as_ref().is_some_and(
                    |fb| matches!(&fb.buffer, ScanoutBuffer::Gpu(current) if *current == buffer),
                )
            })
        {
            return Err(GpuError::Busy);
        }
        if self.resources[&key].attached {
            if self.raw.resource_detach_backing(id).is_err() {
                // Without a confirmed detach the host may still DMA backing.
                self.mark_lost();
                return Err(GpuError::DeviceLost);
            }
            self.resources.get_mut(&key).unwrap().attached = false;
        }
        if self.raw.resource_unref(id).is_err() {
            // UNREF may have reached the host; reset before dropping backing.
            self.mark_lost();
            return Err(GpuError::DeviceLost);
        }
        self.resources.remove(&key);
        Ok(())
    }

    fn completion_status(&mut self, completion: Completion) -> Result<CompletionStatus, GpuError> {
        self.ensure_ready()?;
        match completion {
            Completion::Complete => Ok(CompletionStatus::Complete),
            Completion::Pending(_) => Err(GpuError::InvalidArgument),
        }
    }

    fn take_irq_endpoint(&mut self) -> Option<Box<dyn GpuIrqEndpoint>> {
        self.irq_endpoint.take()
    }

    fn service_pending(&mut self) -> Result<(), GpuError> {
        self.ensure_ready()?;
        let event = self.raw.ack_interrupt();
        if event.display_changed {
            for index in 0..self.outputs.len() {
                let output = read_output(&mut self.raw, index as u32).map_err(map_error)?;
                if output != self.outputs[index] {
                    self.outputs[index] = output;
                    self.events
                        .push_back(DisplayEvent::OutputChanged(OutputId::new(index as u32)));
                }
            }
        }
        Ok(())
    }

    fn virgl(&mut self) -> Option<&mut dyn VirglOps> {
        (self.raw.has_virgl() || self.raw.has_resource_blob()).then_some(self)
    }
}

impl<H: Hal, T: Transport> VirglOps for VirtIoGpuDevice<H, T> {
    fn command_resource_id(&self, resource: BufferHandle) -> Result<u32, GpuError> {
        self.resource_id(resource)
    }

    fn command_context_id(&self, context: ContextHandle) -> Result<u32, GpuError> {
        self.context_id(context)
    }

    fn create_context(&mut self, name: &str, context_init: u32) -> Result<ContextHandle, GpuError> {
        self.ensure_ready()?;
        let (id, handle) = self.alloc_context()?;
        self.raw
            .ctx_create(id, name, context_init)
            .map_err(map_error)?;
        self.contexts.insert(handle.id().get(), id);
        Ok(handle)
    }

    fn destroy_context(&mut self, context: ContextHandle) -> Result<(), GpuError> {
        self.ensure_ready()?;
        let id = self.context_id(context)?;
        if self
            .attachments
            .iter()
            .any(|(owner, _)| *owner == context.id().get())
        {
            return Err(GpuError::Busy);
        }
        self.raw.ctx_destroy(id).map_err(map_error)?;
        self.contexts.remove(&context.id().get());
        Ok(())
    }

    fn create_resource_3d(
        &mut self,
        desc: Resource3d,
        backing: Option<Arc<dyn Backing>>,
    ) -> Result<BufferHandle, GpuError> {
        self.ensure_ready()?;
        if !self.raw.has_virgl() {
            return Err(GpuError::Unsupported);
        }
        if let Some(backing) = &backing {
            self.validate_backing(backing.as_ref(), backing.len())?;
        }
        let (id, handle) = self.alloc_resource()?;
        if let Err(error) = self.raw.resource_create_3d(ResourceCreate3d {
            ctx_id: 0,
            resource_id: id,
            target: desc.target,
            format: desc.format,
            bind: desc.bind,
            width: desc.width,
            height: desc.height,
            depth: desc.depth,
            array_size: desc.array_size,
            last_level: desc.last_level,
            nr_samples: desc.samples,
            flags: desc.flags,
        }) {
            self.cleanup_failed_create(id);
            return Err(map_error(error));
        }
        if let Some(backing) = &backing
            && let Err(error) = self.attach_backing(id, backing, backing.len())
        {
            self.cleanup_failed_create(id);
            return Err(error);
        }
        self.resources.insert(
            handle.id().get(),
            Resource {
                id,
                backing: backing.clone(),
                kind: ResourceKind::ThreeD {
                    width: desc.width,
                    height: desc.height,
                },
                attached: backing.is_some(),
            },
        );
        Ok(handle)
    }

    fn create_blob(
        &mut self,
        context: Option<ContextHandle>,
        desc: BlobDescriptor,
        backing: Option<Arc<dyn Backing>>,
        initial_commands: &[u8],
    ) -> Result<BufferHandle, GpuError> {
        self.ensure_ready()?;
        let ctx_id = context
            .map(|handle| self.context_id(handle))
            .transpose()?
            .unwrap_or(0);
        let guest_backed = matches!(
            desc.memory,
            crate::BLOB_MEM_GUEST | crate::BLOB_MEM_HOST3D_GUEST
        );
        let host3d = matches!(
            desc.memory,
            crate::BLOB_MEM_HOST3D | crate::BLOB_MEM_HOST3D_GUEST
        );
        if desc.size == 0
            || !matches!(
                desc.memory,
                crate::BLOB_MEM_GUEST | crate::BLOB_MEM_HOST3D | crate::BLOB_MEM_HOST3D_GUEST
            )
            || desc.flags & !crate::BLOB_FLAG_USE_MASK != 0
            || (host3d && ctx_id == 0)
            || (guest_backed != backing.is_some())
            || (!host3d && !initial_commands.is_empty())
        {
            return Err(GpuError::InvalidArgument);
        }
        if desc.flags & crate::BLOB_FLAG_USE_CROSS_DEVICE != 0
            || (host3d && !self.raw.has_virgl())
            || !self.raw.has_resource_blob()
        {
            return Err(GpuError::Unsupported);
        }
        let size = usize::try_from(desc.size).map_err(|_| GpuError::InvalidArgument)?;
        let segments = if let Some(backing) = &backing {
            self.validate_backing(backing.as_ref(), size)?
        } else {
            Vec::new()
        };
        if !initial_commands.is_empty() {
            let next_fence = self
                .next_fence
                .checked_add(1)
                .ok_or(GpuError::OutOfMemory)?;
            self.raw
                .submit_3d(ctx_id, self.next_fence, initial_commands)
                .map_err(map_error)?;
            self.next_fence = next_fence;
        }
        if let Some(backing) = &backing {
            backing.sync_for_device(0..size)?;
        }
        let (id, handle) = self.alloc_resource()?;
        // SAFETY: the backing Arc is retained in the resource table on
        // success. On any failure, UNREF confirmation or device reset precedes
        // its release; validated segments cover the full requested size.
        let result = unsafe {
            self.raw.resource_create_blob(ResourceCreateBlob {
                ctx_id,
                resource_id: id,
                blob_mem: desc.memory,
                blob_flags: desc.flags,
                size: desc.size,
                blob_id: desc.id,
                mem_entries: &segments,
            })
        };
        if let Err(error) = result {
            if !matches!(
                &error,
                Error::Unsupported | Error::InvalidParam | Error::Overflow
            ) {
                self.cleanup_failed_create(id);
            }
            return Err(map_error(error));
        }
        self.resources.insert(
            handle.id().get(),
            Resource {
                id,
                backing,
                kind: ResourceKind::Blob { size: desc.size },
                attached: false,
            },
        );
        Ok(handle)
    }

    fn attach_resource(
        &mut self,
        context: ContextHandle,
        resource: BufferHandle,
    ) -> Result<(), GpuError> {
        self.ensure_ready()?;
        let ctx_id = self.context_id(context)?;
        let resource_id = self.resource_id(resource)?;
        let pair = (context.id().get(), resource.id().get());
        if self.attachments.contains(&pair) {
            return Err(GpuError::Busy);
        }
        self.raw
            .ctx_attach_resource(ctx_id, resource_id)
            .map_err(map_error)?;
        self.attachments.insert(pair);
        Ok(())
    }

    fn detach_resource(
        &mut self,
        context: ContextHandle,
        resource: BufferHandle,
    ) -> Result<(), GpuError> {
        self.ensure_ready()?;
        let ctx_id = self.context_id(context)?;
        let resource_id = self.resource_id(resource)?;
        let pair = (context.id().get(), resource.id().get());
        if !self.attachments.contains(&pair) {
            return Err(GpuError::InvalidHandle);
        }
        self.raw
            .ctx_detach_resource(ctx_id, resource_id)
            .map_err(map_error)?;
        self.attachments.remove(&pair);
        Ok(())
    }

    fn transfer_to_host(&mut self, transfer: Transfer3d) -> Result<Completion, GpuError> {
        self.ensure_ready()?;
        let command = self.transfer_command(transfer)?;
        if let Some(backing) = &self.resources[&transfer.resource.id().get()].backing {
            backing.sync_for_device(0..backing.len())?;
        }
        self.raw.transfer_to_host_3d(command).map_err(map_error)?;
        Ok(Completion::Complete)
    }

    fn transfer_from_host(&mut self, transfer: Transfer3d) -> Result<Completion, GpuError> {
        self.ensure_ready()?;
        let command = self.transfer_command(transfer)?;
        self.raw.transfer_from_host_3d(command).map_err(map_error)?;
        if let Some(backing) = &self.resources[&transfer.resource.id().get()].backing {
            backing.sync_for_cpu(0..backing.len())?;
        }
        Ok(Completion::Complete)
    }

    fn submit(&mut self, context: ContextHandle, commands: &[u8]) -> Result<Completion, GpuError> {
        self.ensure_ready()?;
        let ctx_id = self.context_id(context)?;
        let fence = self.next_fence;
        self.next_fence = fence.checked_add(1).ok_or(GpuError::OutOfMemory)?;
        self.raw
            .submit_3d(ctx_id, fence, commands)
            .map_err(map_error)?;
        Ok(Completion::Complete)
    }

    fn capset_info(&mut self, index: u32) -> Result<CapsetInfo, GpuError> {
        self.ensure_ready()?;
        let info = self.raw.get_capset_info(index).map_err(map_error)?;
        Ok(CapsetInfo {
            id: info.capset_id,
            max_version: info.max_version,
            max_size: info.max_size,
        })
    }

    fn capset(&mut self, id: u32, version: u32, size: u32) -> Result<Vec<u8>, GpuError> {
        self.ensure_ready()?;
        self.raw.get_capset(id, version, size).map_err(map_error)
    }
}

impl<H: Hal, T: Transport> VirtIoGpuDevice<H, T> {
    fn transfer_command(&self, transfer: Transfer3d) -> Result<crate::Transfer3d, GpuError> {
        let ctx_id = self.context_id(transfer.context)?;
        let resource_id = self.resource_id(transfer.resource)?;
        Ok(crate::Transfer3d {
            ctx_id,
            resource_id,
            box_: GpuBox {
                x: transfer.box_.x,
                y: transfer.box_.y,
                z: transfer.box_.z,
                w: transfer.box_.width,
                h: transfer.box_.height,
                d: transfer.box_.depth,
            },
            offset: transfer.offset,
            level: transfer.level,
            stride: transfer.stride,
            layer_stride: transfer.layer_stride,
        })
    }
}

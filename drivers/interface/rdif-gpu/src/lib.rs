#![no_std]

//! Device-scoped GPU resources and optional rendering operations.
//!
//! A GPU device owns hardware resource mappings. Callers own their memory and
//! may share it with the device through [`Backing`]. The driver retains its
//! backing reference until it has confirmed that hardware can no longer use
//! the mapping. All device operations require exclusive access.

extern crate alloc;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{num::NonZeroU64, ops::Range};

pub use dma_api::{DmaAddr, DmaDomainId, DmaSegment};
pub use rdif_base::DriverGeneric;

/// A device-visible allocation whose lifetime is shared with the caller.
///
/// # Safety
///
/// The segments must cover `len()` bytes in order, remain mapped in
/// `domain_id()` for this object's lifetime, and retain stable addresses.
/// No segment may be freed or remapped while a GPU or display device can read
/// it. Implementations must not create overlapping mutable CPU slices and
/// must honor the caller's exclusion of device writes. The sync calls transfer
/// cache ownership for the requested byte range; coherent memory may
/// implement them as no-ops. The driver must reject a backing from another DMA
/// domain or one too short for the requested resource.
pub unsafe trait Backing: Send + Sync {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn domain_id(&self) -> DmaDomainId;
    fn segments(&self) -> &[DmaSegment];
    fn sync_for_device(&self, range: Range<usize>) -> Result<(), GpuError>;
    fn sync_for_cpu(&self, range: Range<usize>) -> Result<(), GpuError>;

    /// Provide exclusive CPU access to a mappable allocation.
    ///
    /// # Safety
    ///
    /// The caller must confirm completion of every GPU operation that may
    /// write this backing, prevent new device writes for the callback's entire
    /// duration, and exclude all other CPU aliases, including user mappings,
    /// that could access the bytes while the mutable slice exists.
    unsafe fn with_cpu_bytes(&self, _access: &mut dyn FnMut(&mut [u8])) -> Result<(), GpuError> {
        Err(GpuError::Unsupported)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb565,
    Rgb888,
    Bgr888,
    Xrgb8888,
    Argb8888,
    Xbgr8888,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb565 => 2,
            Self::Rgb888 | Self::Bgr888 => 3,
            Self::Xrgb8888 | Self::Argb8888 | Self::Xbgr8888 => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferDescriptor {
    Linear {
        size: usize,
    },
    Image2d {
        width: u32,
        height: u32,
        stride: u32,
        format: PixelFormat,
    },
}

impl BufferDescriptor {
    /// Minimum allocation size, including the last row's visible pixels.
    pub fn required_len(self) -> Option<usize> {
        match self {
            Self::Linear { size } => (size > 0).then_some(size),
            Self::Image2d {
                width,
                height,
                stride,
                format,
            } if width > 0 && height > 0 => {
                let row = (width as usize).checked_mul(format.bytes_per_pixel())?;
                if (stride as usize) < row {
                    return None;
                }
                (height as usize - 1)
                    .checked_mul(stride as usize)?
                    .checked_add(row)
            }
            Self::Image2d { .. } => None,
        }
    }
}

/// A handle valid only on the device that created it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferHandle(NonZeroU64);

impl BufferHandle {
    pub const fn new(id: NonZeroU64) -> Self {
        Self(id)
    }

    pub const fn id(self) -> NonZeroU64 {
        self.0
    }
}

/// A context handle valid only on the device that created it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContextHandle(NonZeroU64);

impl ContextHandle {
    pub const fn new(id: NonZeroU64) -> Self {
        Self(id)
    }

    pub const fn id(self) -> NonZeroU64 {
        self.0
    }
}

/// Completion of a submitted operation. `Complete` is a synchronous result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Completion {
    Complete,
    Pending(NonZeroU64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionStatus {
    Pending,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuIrqEvent {
    pub handled: bool,
    pub work_pending: bool,
}

/// Movable interrupt endpoint. It must never acquire the GPU control lock or
/// wait for a queue command; it only acknowledges a source and publishes work
/// for [`GpuDevice::service_pending`] in task context.
pub trait GpuIrqEndpoint: Send + Sync {
    fn handle_irq(&self) -> GpuIrqEvent;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuCapabilities {
    pub dma_domain: DmaDomainId,
    pub supports_image_2d: bool,
    pub supports_3d: bool,
    pub supports_blob: bool,
    pub supports_context_init: bool,
}

/// Bus data sufficient for an OS to construct its own device identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusIdentity {
    Pci(PciIdentity),
    Platform {
        name: String,
        compatible: Option<String>,
    },
    Virtual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciIdentity {
    pub vendor: u16,
    pub device: u16,
    pub subsystem_vendor: u16,
    pub subsystem_device: u16,
    pub revision: u8,
    pub class: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuIdentity {
    pub driver_name: String,
    /// Stable instance name supplied by device registration, such as
    /// `virtio-gpu0`. The operating system chooses how to expose it.
    pub device_name: String,
    pub driver_version: DriverVersion,
    pub description: String,
    pub bus: BusIdentity,
    pub modalias: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuError {
    #[error("operation not supported")]
    Unsupported,
    #[error("device is not available")]
    NotAvailable,
    #[error("invalid argument")]
    InvalidArgument,
    #[error("invalid or stale device handle")]
    InvalidHandle,
    #[error("device is busy")]
    Busy,
    #[error("device is not ready")]
    NotReady,
    #[error("out of memory")]
    OutOfMemory,
    #[error("device was lost")]
    DeviceLost,
    #[error("device I/O failed")]
    Io,
}

/// GPU resource operations. Implementations validate every device-local
/// handle, including handles originating from another device or generation.
pub trait GpuDevice: DriverGeneric {
    fn identity(&self) -> GpuIdentity;
    fn capabilities(&self) -> GpuCapabilities;

    /// On failure the device must not retain `backing` or publish a resource.
    /// A successful resource retains its backing until `release_buffer` has
    /// confirmed it is no longer referenced by hardware, contexts or scanout.
    fn create_buffer(
        &mut self,
        desc: BufferDescriptor,
        backing: Arc<dyn Backing>,
    ) -> Result<BufferHandle, GpuError>;

    /// Obtain a shared reference for CPU mapping or an OS-owned handle.
    /// Host-only resources return `None`; unknown handles return an error.
    fn buffer_backing(&self, buffer: BufferHandle) -> Result<Option<Arc<dyn Backing>>, GpuError>;

    /// Fails with `Busy` while context, scanout or in-flight work still uses
    /// the buffer; its backing remains owned by the device in that case.
    fn release_buffer(&mut self, buffer: BufferHandle) -> Result<(), GpuError>;

    fn completion_status(&mut self, completion: Completion) -> Result<CompletionStatus, GpuError>;

    /// Take the single independently movable IRQ endpoint, if present.
    fn take_irq_endpoint(&mut self) -> Option<Box<dyn GpuIrqEndpoint>> {
        None
    }

    /// Drain published IRQ work and advance completions in task context.
    fn service_pending(&mut self) -> Result<(), GpuError> {
        Ok(())
    }

    /// Protocol extension, present when 3D rendering or blob resources were
    /// negotiated. Individual methods may return `Unsupported`.
    fn virgl(&mut self) -> Option<&mut dyn VirglOps> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapsetInfo {
    pub id: u32,
    pub max_version: u32,
    pub max_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resource3d {
    pub target: u32,
    pub format: u32,
    pub bind: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub array_size: u32,
    pub last_level: u32,
    pub samples: u32,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobDescriptor {
    pub memory: u32,
    pub flags: u32,
    pub size: u64,
    pub id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferBox {
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transfer3d {
    pub resource: BufferHandle,
    pub context: ContextHandle,
    pub box_: TransferBox,
    pub offset: u64,
    pub level: u32,
    pub stride: u32,
    pub layer_stride: u32,
}

/// Optional 3D rendering operations. The GPU still owns every returned
/// resource; the OS adapter translates Linux driver UAPI outside this trait.
pub trait VirglOps {
    /// Translate an owned resource into the ID embedded in this device's
    /// guest command stream. Reject a handle from another device or one that
    /// has already been released. This ID is not a generic GPU handle.
    fn command_resource_id(&self, resource: BufferHandle) -> Result<u32, GpuError>;

    /// Translate a context into the ID expected by the guest command stream.
    fn command_context_id(&self, context: ContextHandle) -> Result<u32, GpuError>;

    fn create_context(&mut self, name: &str, context_init: u32) -> Result<ContextHandle, GpuError>;
    fn destroy_context(&mut self, context: ContextHandle) -> Result<(), GpuError>;
    fn create_resource_3d(
        &mut self,
        desc: Resource3d,
        backing: Option<Arc<dyn Backing>>,
    ) -> Result<BufferHandle, GpuError>;
    fn create_blob(
        &mut self,
        context: Option<ContextHandle>,
        desc: BlobDescriptor,
        backing: Option<Arc<dyn Backing>>,
        initial_commands: &[u8],
    ) -> Result<BufferHandle, GpuError>;
    fn attach_resource(
        &mut self,
        context: ContextHandle,
        resource: BufferHandle,
    ) -> Result<(), GpuError>;
    fn detach_resource(
        &mut self,
        context: ContextHandle,
        resource: BufferHandle,
    ) -> Result<(), GpuError>;
    fn transfer_to_host(&mut self, transfer: Transfer3d) -> Result<Completion, GpuError>;
    fn transfer_from_host(&mut self, transfer: Transfer3d) -> Result<Completion, GpuError>;
    fn submit(&mut self, context: ContextHandle, commands: &[u8]) -> Result<Completion, GpuError>;
    fn capset_info(&mut self, index: u32) -> Result<CapsetInfo, GpuError>;
    fn capset(&mut self, id: u32, version: u32, size: u32) -> Result<Vec<u8>, GpuError>;
}

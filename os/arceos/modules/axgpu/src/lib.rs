//! Single ArceOS owner of a registered GPU and its optional display outputs.

#![no_std]

extern crate alloc;

mod backing;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_lazyinit::LazyInit;
use ax_task::sync::Mutex;
use backing::DmaFramebuffer;
use dma_api::DeviceDma;
use irq_framework::IrqId;
pub use rdif_display;
use rdif_display::{
    DisplayError, DisplayState, Framebuffer, GpuDisplay, OutputId, Rect, ScanoutBuffer,
};
pub use rdif_gpu;
use rdif_gpu::{
    Backing, BufferDescriptor, GpuCapabilities, GpuDevice, GpuError, GpuIdentity, GpuIrqEndpoint,
    PixelFormat,
};

/// One device object. Headless GPUs do not need to implement display control.
pub enum ErasedGpuDevice {
    GpuOnly(Box<dyn GpuDevice>),
    WithDisplay(Box<dyn GpuDisplay>),
}

impl ErasedGpuDevice {
    fn gpu(&mut self) -> &mut dyn GpuDevice {
        match self {
            Self::GpuOnly(device) => device.as_mut(),
            Self::WithDisplay(device) => device.as_mut(),
        }
    }

    fn display(&mut self) -> Option<&mut dyn GpuDisplay> {
        match self {
            Self::GpuOnly(_) => None,
            Self::WithDisplay(device) => Some(device.as_mut()),
        }
    }
}

/// Prepared device and its DMA domain supplied by platform discovery.
pub struct GpuRegistration {
    pub device: ErasedGpuDevice,
    pub dma: DeviceDma,
    pub irq: Option<IrqId>,
}

struct GpuRuntime {
    device: ErasedGpuDevice,
    /// Discovered devices that are not active yet must keep their transport
    /// queues and DMA allocations alive until a coordinated shutdown exists.
    retained_devices: Vec<GpuRegistration>,
    dma: DeviceDma,
    irq: Option<IrqId>,
    default_scanout: Option<DisplayState>,
    default_mapping: Option<Arc<MappableBacking>>,
}

/// OS mapping information for an allocation from the registered GPU's DMA
/// domain. `physical` names CPU physical pages; the RDIF backing's segments
/// may instead contain translated device addresses.
pub struct MappableBacking {
    backing: Arc<DmaFramebuffer>,
    physical: ax_memory_addr::PhysAddrRange,
}

impl MappableBacking {
    fn new(backing: Arc<DmaFramebuffer>) -> Self {
        let physical = backing.physical_range();
        Self { backing, physical }
    }

    /// Share the DMA allocation with a device resource. A caller that
    /// submitted a device-to-guest write waits for its completion before
    /// requesting CPU access to these bytes.
    pub fn backing(&self) -> Arc<dyn Backing> {
        self.backing.clone()
    }

    pub fn physical(&self) -> ax_memory_addr::PhysAddrRange {
        self.physical
    }

    /// Copies bytes from a CPU-mappable backing under the GPU control lock.
    /// A prior device write must have reached its completion token first.
    pub fn read_at(&self, output: &mut [u8], offset: usize) -> Result<usize, GpuError> {
        with_gpu(|_| self.backing.read_at(output, offset))
    }

    /// Copies bytes to a CPU-mappable backing under the GPU control lock.
    /// It may race with userspace's own mmap writes, as framebuffer writes do.
    pub fn write_at(&self, input: &[u8], offset: usize) -> Result<usize, GpuError> {
        with_gpu(|_| self.backing.write_at(input, offset))?
    }
}

static MAIN_GPU: LazyInit<Mutex<GpuRuntime>> = LazyInit::new();
static GPU_IRQ_ENDPOINT: LazyInit<Box<dyn GpuIrqEndpoint>> = LazyInit::new();
static GPU_IRQ_ENABLED: AtomicBool = AtomicBool::new(false);
static GPU_WORK_PENDING: AtomicBool = AtomicBool::new(false);
static GPU_WORK_NOTIFY: LazyInit<fn()> = LazyInit::new();

/// Registers one GPU. The first discovered device is the active instance.
/// A failed default scanout leaves GPU rendering available.
pub fn init_gpu(devices: impl IntoIterator<Item = GpuRegistration>) {
    let mut devices = devices.into_iter();
    let Some(mut registration) = devices.next() else {
        log::warn!("No GPU device found");
        return;
    };
    let retained_devices = devices.collect();
    let identity = registration.device.gpu().identity();
    log::info!("Use GPU device: {}", identity.device_name);

    let endpoint = registration.device.gpu().take_irq_endpoint();
    let default_scanout = match registration.device.display() {
        Some(device) => match create_default_scanout(device, &registration.dma) {
            Ok(state) => state,
            Err(error) => {
                log::warn!("GPU display initialization failed: {error}");
                None
            }
        },
        None => None,
    };

    let (default_scanout, default_mapping) = match default_scanout {
        Some((state, mapping)) => (Some(state), Some(mapping)),
        None => (None, None),
    };
    MAIN_GPU.init_once(Mutex::new(GpuRuntime {
        device: registration.device,
        retained_devices,
        dma: registration.dma,
        irq: registration.irq,
        default_scanout,
        default_mapping,
    }));
    if let Some(endpoint) = endpoint {
        GPU_IRQ_ENDPOINT.init_once(endpoint);
    }
}

fn create_default_scanout(
    device: &mut dyn GpuDisplay,
    dma: &DeviceDma,
) -> Result<Option<(DisplayState, Arc<MappableBacking>)>, DisplayError> {
    let output = (0..device.output_count())
        .map(OutputId::new)
        .find_map(|id| device.output(id).ok().filter(|info| info.connected))
        .ok_or(DisplayError::NotAvailable)?;
    let Some(mode) = output
        .preferred_mode
        .or_else(|| output.modes.first().copied())
    else {
        return Ok(None);
    };
    let mut formats = output.formats;
    formats.sort_by_key(|format| u8::from(*format != PixelFormat::Xrgb8888));
    for format in formats {
        let stride = mode
            .width
            .checked_mul(format.bytes_per_pixel() as u32)
            .ok_or(DisplayError::InvalidState)?;
        let size = (stride as usize)
            .checked_mul(mode.height as usize)
            .and_then(NonZeroUsize::new)
            .ok_or(DisplayError::InvalidState)?;
        let mapping = Arc::new(MappableBacking::new(Arc::new(DmaFramebuffer::allocate(
            dma, size,
        )?)));
        let buffer = match device.create_buffer(
            BufferDescriptor::Image2d {
                width: mode.width,
                height: mode.height,
                stride,
                format,
            },
            mapping.backing(),
        ) {
            Ok(handle) => ScanoutBuffer::Gpu(handle),
            Err(GpuError::Unsupported) => ScanoutBuffer::Backing(mapping.backing()),
            Err(error) => return Err(error.into()),
        };
        let state = DisplayState {
            output: output.id,
            mode: Some(mode),
            framebuffer: Some(Framebuffer {
                buffer: buffer.clone(),
                width: mode.width,
                height: mode.height,
                stride,
                offset: 0,
                format,
            }),
            damage: alloc::vec![Rect {
                x: 0,
                y: 0,
                width: mode.width,
                height: mode.height,
            }],
        };
        if let Err(error) = device.check(&state) {
            if let ScanoutBuffer::Gpu(handle) = buffer {
                device.release_buffer(handle)?;
            }
            if matches!(
                error,
                DisplayError::Unsupported | DisplayError::InvalidState
            ) {
                continue;
            }
            return Err(error);
        }
        if let Err(error) = device.commit(&state) {
            if let ScanoutBuffer::Gpu(handle) = buffer {
                device.release_buffer(handle)?;
            }
            return Err(error);
        }
        return Ok(Some((state, mapping)));
    }
    Err(DisplayError::Unsupported)
}

/// Whether a GPU was registered, including a headless rendering device.
pub fn has_gpu() -> bool {
    MAIN_GPU.is_inited()
}

/// Whether the active GPU exposes a display controller with output slots.
/// A boot framebuffer is optional and does not determine KMS capability.
pub fn has_display_controller() -> bool {
    if !has_gpu() {
        return false;
    }
    let mut runtime = MAIN_GPU.lock();
    runtime
        .device
        .display()
        .is_some_and(|device| device.output_count() != 0)
}

/// Enumerates discovered devices without selecting additional active owners.
pub fn device_identities() -> Vec<GpuIdentity> {
    if !has_gpu() {
        return Vec::new();
    }
    let mut runtime = MAIN_GPU.lock();
    let mut identities = Vec::with_capacity(runtime.retained_devices.len() + 1);
    identities.push(runtime.device.gpu().identity());
    for registration in &mut runtime.retained_devices {
        identities.push(registration.device.gpu().identity());
    }
    identities
}

/// Identity of the bound device; names and bus attributes come from its driver.
pub fn identity() -> Option<GpuIdentity> {
    MAIN_GPU
        .is_inited()
        .then(|| MAIN_GPU.lock().device.gpu().identity())
}

/// Negotiated capabilities of the bound GPU.
pub fn capabilities() -> Option<GpuCapabilities> {
    MAIN_GPU
        .is_inited()
        .then(|| MAIN_GPU.lock().device.gpu().capabilities())
}

/// Allocates a CPU-mappable buffer in the active GPU's DMA domain.
/// Its ownership can be shared with a GPU resource or a userspace mapping.
pub fn allocate_backing(len: NonZeroUsize) -> Result<Arc<dyn Backing>, GpuError> {
    Ok(allocate_mappable_backing(len)?.backing())
}

/// Allocates a DMA backing and retains its CPU physical mapping for mmap.
pub fn allocate_mappable_backing(len: NonZeroUsize) -> Result<Arc<MappableBacking>, GpuError> {
    if !has_gpu() {
        return Err(GpuError::NotAvailable);
    }
    let runtime = MAIN_GPU.lock();
    let backing = Arc::new(DmaFramebuffer::allocate(&runtime.dma, len)?);
    Ok(Arc::new(MappableBacking::new(backing)))
}

/// The boot framebuffer and its CPU physical mapping remain owned by the
/// runtime even when an application presents a different scanout resource.
pub fn default_framebuffer_mapping() -> Result<(DisplayState, Arc<MappableBacking>), DisplayError> {
    if !has_gpu() {
        return Err(DisplayError::NotAvailable);
    }
    let runtime = MAIN_GPU.lock();
    let state = runtime
        .default_scanout
        .clone()
        .ok_or(DisplayError::NotAvailable)?;
    let mapping = runtime
        .default_mapping
        .clone()
        .ok_or(DisplayError::NotAvailable)?;
    Ok((state, mapping))
}

/// Accesses GPU resources under the single sleepable device lock.
/// The closure must not recursively enter `ax-gpu` or retain device borrows.
pub fn with_gpu<R>(access: impl FnOnce(&mut dyn GpuDevice) -> R) -> Result<R, GpuError> {
    if !has_gpu() {
        return Err(GpuError::NotAvailable);
    }
    let mut runtime = MAIN_GPU.lock();
    service_pending(&mut runtime)?;
    Ok(access(runtime.device.gpu()))
}

/// Accesses the display capability of the same registered GPU.
pub fn with_display<R>(access: impl FnOnce(&mut dyn GpuDisplay) -> R) -> Result<R, DisplayError> {
    if !has_gpu() {
        return Err(DisplayError::NotAvailable);
    }
    let mut runtime = MAIN_GPU.lock();
    service_pending(&mut runtime)?;
    let device = runtime.device.display().ok_or(DisplayError::NotAvailable)?;
    Ok(access(device))
}

/// Returns the active framebuffer backing, retaining its memory after the
/// device lock is released. The caller still needs to coordinate CPU writes
/// with GPU submissions through `with_display`.
pub fn framebuffer_backing() -> Result<Arc<dyn Backing>, DisplayError> {
    with_display(|device| {
        for id in (0..device.output_count()).map(OutputId::new) {
            let Some(state) = device.current_state(id)? else {
                continue;
            };
            let Some(framebuffer) = state.framebuffer else {
                continue;
            };
            return match framebuffer.buffer {
                ScanoutBuffer::Backing(backing) => Ok(backing),
                ScanoutBuffer::Gpu(buffer) => device
                    .buffer_backing(buffer)?
                    .ok_or(DisplayError::NotAvailable),
            };
        }
        Err(DisplayError::NotAvailable)
    })?
}

/// Rebinds the boot framebuffer after another scanout has been displayed.
pub fn restore_default_scanout() -> Result<(), DisplayError> {
    if !has_gpu() {
        return Err(DisplayError::NotAvailable);
    }
    let mut runtime = MAIN_GPU.lock();
    service_pending(&mut runtime)?;
    let state = runtime
        .default_scanout
        .clone()
        .ok_or(DisplayError::NotAvailable)?;
    runtime
        .device
        .display()
        .ok_or(DisplayError::NotAvailable)?
        .commit(&state)?;
    Ok(())
}

fn service_pending(runtime: &mut GpuRuntime) -> Result<(), GpuError> {
    if GPU_WORK_PENDING.swap(false, Ordering::AcqRel)
        && let Err(error) = runtime.device.gpu().service_pending()
    {
        GPU_WORK_PENDING.store(true, Ordering::Release);
        return Err(error);
    }
    Ok(())
}

/// Installs a task worker notification after the scheduler is online.
/// The callback must be safe in hard IRQ context and must not take the GPU lock.
pub fn set_irq_work_notifier(notify: fn()) {
    GPU_WORK_NOTIFY.init_once(notify);
}

/// Advances transport acknowledgements and device events in task context.
pub fn service_irq_work() -> Result<(), GpuError> {
    if !has_gpu() {
        return Ok(());
    }
    service_pending(&mut MAIN_GPU.lock())
}

/// Resolves the optional GPU interrupt assigned by the platform runtime.
pub fn irq_id() -> Option<IrqId> {
    has_gpu().then(|| MAIN_GPU.lock().irq).flatten()
}

pub fn enable_irq() {
    GPU_IRQ_ENABLED.store(true, Ordering::Release);
}

pub fn disable_irq() {
    GPU_IRQ_ENABLED.store(false, Ordering::Release);
}

/// Hard IRQ entry. It never takes the sleepable GPU control lock.
pub fn handle_irq() -> bool {
    if !GPU_IRQ_ENABLED.load(Ordering::Acquire) || !GPU_IRQ_ENDPOINT.is_inited() {
        return false;
    }
    let event = GPU_IRQ_ENDPOINT.handle_irq();
    if event.work_pending {
        GPU_WORK_PENDING.store(true, Ordering::Release);
        if GPU_WORK_NOTIFY.is_inited() {
            (*GPU_WORK_NOTIFY)();
        }
    }
    event.handled
}

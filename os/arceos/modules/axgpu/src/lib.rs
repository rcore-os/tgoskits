//! Single ArceOS owner of a registered GPU and its optional display outputs.

#![no_std]

extern crate alloc;

mod backing;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    cell::Cell,
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_lazyinit::LazyInit;
use ax_task::sync::{Mutex, WaitQueue};
use backing::DmaFramebuffer;
use dma_api::DeviceDma;
use irq_framework::IrqId;
pub use rdif_display;
use rdif_display::{
    DisplayError, DisplayState, Framebuffer, GpuDisplay, OutputId, Rect, ScanoutBuffer,
};
pub use rdif_gpu;
use rdif_gpu::{
    Backing, BufferDescriptor, Completion, CompletionStatus, GpuCapabilities, GpuDevice, GpuError,
    GpuIdentity, GpuIrqEndpoint, PixelFormat,
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
static GPU_COMPLETION_NOTIFY: LazyInit<fn()> = LazyInit::new();

/// Task-context waiters for GPU completion progress, woken by
/// [`notify_completions`] after every completion pump. Waiters re-check
/// their condition under `MAIN_GPU` on every wake; see
/// [`wait_gpu_condition`].
static GPU_WAIT_QUEUE: WaitQueue = WaitQueue::new();

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
                let completion = device.release_buffer(handle)?;
                // The mapping leaves scope below: observe the fenced unref's
                // completion before the DMA goes back to the allocator.
                wait_rollback_completion_exclusive(device, completion);
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
                let completion = device.release_buffer(handle)?;
                wait_rollback_completion_exclusive(device, completion);
            }
            return Err(error);
        }
        return Ok(Some((state, mapping)));
    }
    Err(DisplayError::Unsupported)
}

/// Waits for a rollback `release_buffer` completion while the device is
/// registration-exclusive — not yet published in `MAIN_GPU`, so the
/// `with_gpu*` accessors, the wait queue and the IRQ worker cannot reach
/// it, and nothing else pumps this device. The poll works because
/// `completion_status` itself delivers the accumulated batch and pumps
/// (`GpuDevice::completion_status` contract), and the virtual device
/// services the kick synchronously, so the first check already observes
/// `Complete`; the bounded burn only matters for a backlogged host. Like
/// the probe phase of [`wait_gpu_condition`], the bound is a fixed
/// [`WAIT_PROBE_ROUNDS`] round budget rather than a wall-clock timeout. On
/// expiry the backing is released while the host may still DMA it — the
/// same accepted tradeoff the driver documents for its bounded waits, here
/// on a boot-time rollback path for a scanout state that already failed
/// validation.
fn wait_rollback_completion_exclusive<D: GpuDevice + ?Sized>(
    device: &mut D,
    completion: Completion,
) {
    let fence = match completion {
        Completion::Complete => return,
        Completion::Pending(fence) => fence,
    };
    for round in 0..WAIT_PROBE_ROUNDS {
        match device.completion_status(Completion::Pending(fence)) {
            Ok(CompletionStatus::Complete) => return,
            Ok(CompletionStatus::Pending) => {
                if round + 1 == WAIT_PROBE_ROUNDS {
                    log::warn!(
                        "ax-gpu: rollback release completion unconfirmed; the backing is released \
                         while the host may still DMA it"
                    );
                }
                core::hint::spin_loop();
            }
            // The device is failing anyway (this path only runs after a
            // failed scanout check or commit); the caller's original error
            // carries the failure.
            Err(_) => return,
        }
    }
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
    let result = access(runtime.device.gpu());
    // The access may have pumped completions through its own queries
    // (`fence_completed` and friends deliver and pump), which the
    // IRQ-worker-only notify would miss in a polling environment — a
    // sleeping waiter cannot self-pump, so it would ride its deadline.
    // Wake here too; near-free when nobody waits.
    notify_completions();
    Ok(result)
}

/// Allows resource teardown while deferred device work reports an error.
/// Callers must only complete or release resources, never start new work.
pub fn with_gpu_for_cleanup<R>(
    access: impl FnOnce(&mut dyn GpuDevice) -> R,
) -> Result<R, GpuError> {
    if !has_gpu() {
        return Err(GpuError::NotAvailable);
    }
    let mut runtime = MAIN_GPU.lock();
    service_pending_for_access(&mut runtime);
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

/// Allows a display cleanup operation while an output-change query is pending.
/// Callers must not use stale output information to select a new scanout.
pub fn with_display_for_cleanup<R>(
    access: impl FnOnce(&mut dyn GpuDisplay) -> R,
) -> Result<R, DisplayError> {
    if !has_gpu() {
        return Err(DisplayError::NotAvailable);
    }
    let mut runtime = MAIN_GPU.lock();
    service_pending_for_access(&mut runtime);
    let device = runtime.device.display().ok_or(DisplayError::NotAvailable)?;
    Ok(access(device))
}

/// How long a lock-free GPU completion wait trusts the host before reporting
/// [`GpuError::TimedOut`] — the same bound the driver applies to its own
/// in-lock spins (`WAIT_TIMEOUT_NS`).
pub const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Probe rounds before sleeping: the virtual device services the virtqueue
/// kick synchronously inside the MMIO write and the observed fence latency
/// of this stack spans from the idle low hundreds of microseconds up to
/// tens of milliseconds when the host is backlogged with earlier batches —
/// a per-frame wait that outlives the probe budget pays one sleep-wake hop,
/// which showed up as -40..-80% on the heaviest glmark2 scenes at a 4096
/// budget. Each probe re-takes and drops the lock, so producers and the IRQ
/// worker interleave freely; the bound only matters on a genuinely stalled
/// host, which falls through to the sleep after a bounded burn.
const WAIT_PROBE_ROUNDS: usize = 32768;

/// Waits for `cond` to hold on the GPU runtime, sleeping outside the device
/// lock — the stack's `wait_event` equivalent, with a bounded adaptive
/// probe in front.
///
/// Timeout semantics: `timeout` bounds the *sleep* phase (measured from the
/// moment the sleep starts). The probe phase in front of it is bounded
/// separately, by the fixed [`WAIT_PROBE_ROUNDS`] round budget, so the
/// caller's worst-case wait is *probe burn + timeout*; with short timeouts
/// the probe budget can dominate. Every in-tree caller passes
/// [`GPU_WAIT_TIMEOUT`], against which the probe burn is noise.
///
/// Three correctness arguments carry the whole design:
///
/// * The probe phase self-pumps under per-round locking, so progress that
///   is already observable resolves without any wake chain; the budget
///   cannot wedge a producer because every round releases the lock.
/// * The waiter sleeps only after the condition was checked under and the
///   lock has been released, so the completion pump (the IRQ worker or any
///   `with_gpu` caller) can always take `MAIN_GPU` between two checks:
///   progress never depends on the waiter. A woken waiter re-takes the lock
///   and re-checks, so there is no lock-order inversion —
///   [`notify_completions`] runs while a pump holds `MAIN_GPU`, and
///   `WaitQueue::notify_all` takes no lock a waiter holds.
/// * The condition observes device state through the driver's polling
///   queries (`fence_completed`), which deliver the accumulated batch and
///   pump completions themselves: the probe drives its own progress exactly
///   like an IRQ-driven environment. In a polling-only environment a waiter
///   that reaches the sleep phase depends on another task's access for its
///   wake — known limitation; every shipping configuration has completion
///   IRQs or concurrent pollers.
///
/// `cond` runs once per wake-up under `MAIN_GPU`: it must not sleep, must
/// not re-enter this crate, and should only use the device's query methods.
fn wait_gpu_condition(
    timeout: Duration,
    cond: impl Fn(&mut GpuRuntime) -> Result<bool, GpuError>,
) -> Result<(), GpuError> {
    if !has_gpu() {
        return Err(GpuError::NotAvailable);
    }
    for _ in 0..WAIT_PROBE_ROUNDS {
        let mut runtime = MAIN_GPU.lock();
        let probed = match service_pending(&mut runtime) {
            Err(error) => return Err(error),
            Ok(()) => cond(&mut runtime),
        };
        match probed {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => return Err(error),
        }
    }
    let cond_failed: Cell<Option<GpuError>> = Cell::new(None);
    let timed_out = GPU_WAIT_QUEUE.wait_timeout_until(timeout, || {
        let mut runtime = MAIN_GPU.lock();
        if let Err(error) = service_pending(&mut runtime) {
            // The pump itself failed: leave the wait, report it below.
            cond_failed.set(Some(error));
            return true;
        }
        match cond(&mut runtime) {
            Ok(done) => done,
            Err(error) => {
                cond_failed.set(Some(error));
                true
            }
        }
    });
    if let Some(error) = cond_failed.take() {
        return Err(error);
    }
    if timed_out {
        log::warn!(
            "ax-gpu: completion wait timed out after {timeout:?} (waiter retried or gave up)"
        );
        return Err(GpuError::TimedOut);
    }
    Ok(())
}

/// Waits, outside the device control lock, for the virgl fence `fence` and
/// everything submitted before it to complete. The virgl-scoped counterpart
/// of [`wait_completion`]: a submit fence only exists on a 3D device, so the
/// query goes through [`VirglOps::fence_completed`]. Task context only. A
/// stalled host returns [`GpuError::TimedOut`] and stays usable; the caller
/// may retry.
///
/// `timeout` bounds the sleep phase; the probe phase in front of it is a
/// separate fixed budget (see `wait_gpu_condition`), so the worst case is
/// probe burn + `timeout`.
pub fn virgl_wait_fence(fence: u64, timeout: Duration) -> Result<(), GpuError> {
    wait_gpu_condition(timeout, |runtime| match runtime.device.gpu().virgl() {
        Some(virgl) => virgl.fence_completed(fence),
        None => Err(GpuError::Unsupported),
    })
}

/// Waits, outside the device control lock, for a fenced fire-and-forget
/// completion (`submit`, `transfer_from_host`, `release_buffer`) to be
/// observed. Capability-independent: the fenced command completes on the
/// control queue like every other one, so this works on a plain 2D device
/// without virgl just as on a 3D one. A stalled host returns
/// [`GpuError::TimedOut`] and stays usable; the caller may retry. The
/// virgl-scoped counterpart for a raw submit fence is [`virgl_wait_fence`].
///
/// `timeout` bounds the sleep phase; the probe phase in front of it is a
/// separate fixed budget (see `wait_gpu_condition`), so the worst case is
/// probe burn + `timeout`.
pub fn wait_completion(completion: Completion, timeout: Duration) -> Result<(), GpuError> {
    match completion {
        Completion::Complete => Ok(()),
        Completion::Pending(fence) => wait_gpu_condition(timeout, |runtime| {
            Ok(matches!(
                runtime
                    .device
                    .gpu()
                    .completion_status(Completion::Pending(fence))?,
                CompletionStatus::Complete
            ))
        }),
    }
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
    // The present completion needs no observation here: the boot
    // framebuffer's mapping is retained in `GpuRuntime` for the runtime's
    // lifetime, so no backing is released that the host could still DMA.
    let _ = runtime
        .device
        .display()
        .ok_or(DisplayError::NotAvailable)?
        .commit(&state)?;
    Ok(())
}

fn service_pending_for_access(runtime: &mut GpuRuntime) {
    // A display event query may fail while resource cleanup still needs the
    // device. Keep the event pending for the IRQ worker without blocking access.
    if let Err(error) = service_pending(runtime) {
        log::debug!("GPU event service deferred: {error}");
    }
}

fn service_pending(runtime: &mut GpuRuntime) -> Result<(), GpuError> {
    if GPU_WORK_PENDING.swap(false, Ordering::AcqRel) {
        if let Err(error) = runtime.device.gpu().service_pending() {
            GPU_WORK_PENDING.store(true, Ordering::Release);
            return Err(error);
        }
        // Completions just became observable at the device level: let the
        // registered notifier wake whoever polls on them now instead of at
        // the OS's next periodic scan.
        notify_completions();
    }
    Ok(())
}

/// Installs a task worker notification after the scheduler is online.
/// The callback must be safe in hard IRQ context and must not take the GPU lock.
pub fn set_irq_work_notifier(notify: fn()) {
    GPU_WORK_NOTIFY.init_once(notify);
}

/// Installs the completion notifier (once, after the scheduler is online).
/// It runs in task context right after the device service pumped completions,
/// while the caller still holds the GPU control lock: it must be cheap and
/// must NOT take the GPU lock again.
pub fn set_completion_notifier(notify: fn()) {
    GPU_COMPLETION_NOTIFY.init_once(notify);
}

fn notify_completions() {
    // Wake the lock-free waiters first: the generation-checked queue closes
    // the check-then-park window in `wait_gpu_condition` against this
    // notify. This runs while the pump holds `MAIN_GPU`, which is safe
    // because `notify_all` never takes that lock — woken waiters queue on
    // it and re-check once the pumping transaction ends.
    GPU_WAIT_QUEUE.notify_all();
    if GPU_COMPLETION_NOTIFY.is_inited() {
        (*GPU_COMPLETION_NOTIFY)();
    }
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

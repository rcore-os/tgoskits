//! [ArceOS](https://github.com/arceos-org/arceos) display module.
//!
//! Currently only supports direct writing to the framebuffer.

#![no_std]

extern crate alloc;

mod device;
pub mod rdif;
mod types;

use core::sync::atomic::{AtomicU64, Ordering};

use ax_lazyinit::LazyInit;
use ax_task::sync::{SpinLock as Mutex, SpinLockGuard};
pub use device::{DisplayDevice, DisplayError, DisplayResult, ErasedDisplayDevice, Gpu3dErrorKind};
pub use types::{CapsetInfo, DisplayInfo, PixelFormat, TransferBox};

static MAIN_DISPLAY: LazyInit<Mutex<ErasedDisplayDevice>> = LazyInit::new();

// ---- lock-wait instrumentation (2026-08-27: quantify MAIN_DISPLAY spin
// contention on the multi-vCPU display path). Every `gpu3d_*` entry acquires
// the same global spin lock; the wait time here is the cross-vCPU serialization
// cost (Linux holds no device lock while waiting for host completion, so this
// number should be ~0 there). Consumed by the card0 perf report. ----
static LOCK_WAIT_NS: AtomicU64 = AtomicU64::new(0);
static LOCK_WAIT_CNT: AtomicU64 = AtomicU64::new(0);
static LOCK_WAIT_MAX_NS: AtomicU64 = AtomicU64::new(0);

#[inline]
fn lock_display() -> SpinLockGuard<'static, ErasedDisplayDevice> {
    let t0 = ax_hal::time::monotonic_time_nanos();
    let guard = MAIN_DISPLAY.lock();
    let dt = ax_hal::time::monotonic_time_nanos().saturating_sub(t0);
    LOCK_WAIT_NS.fetch_add(dt, Ordering::Relaxed);
    LOCK_WAIT_CNT.fetch_add(1, Ordering::Relaxed);
    let mut max = LOCK_WAIT_MAX_NS.load(Ordering::Relaxed);
    while dt > max {
        match LOCK_WAIT_MAX_NS.compare_exchange_weak(max, dt, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(actual) => max = actual,
        }
    }
    guard
}

/// Lock-wait statistics since the last reset: (count, total_ns, max_ns).
/// Counts every `gpu3d_*` entry's spin before the global display lock.
pub fn gpu3d_lock_wait_stats() -> (u64, u64, u64) {
    (
        LOCK_WAIT_CNT.load(Ordering::Relaxed),
        LOCK_WAIT_NS.load(Ordering::Relaxed),
        LOCK_WAIT_MAX_NS.load(Ordering::Relaxed),
    )
}

/// Reset the lock-wait statistics (called at each card0 perf-report window).
pub fn gpu3d_lock_wait_reset() {
    LOCK_WAIT_CNT.store(0, Ordering::Relaxed);
    LOCK_WAIT_NS.store(0, Ordering::Relaxed);
    LOCK_WAIT_MAX_NS.store(0, Ordering::Relaxed);
}

/// Initializes the display subsystem by underlayer devices.
pub fn init_display(display_devs: impl IntoIterator<Item = ErasedDisplayDevice>) {
    log::info!("Initialize display subsystem...");

    if let Some(dev) = display_devs.into_iter().next() {
        log::info!("  use display device 0: {}", dev.name());
        MAIN_DISPLAY.init_once(Mutex::new(dev));
    } else {
        log::warn!("  No display device found!");
    }
}

/// Checks if there is a display device.
pub fn has_display() -> bool {
    MAIN_DISPLAY.is_inited()
}

/// Gets the framebuffer information.
pub fn framebuffer_info() -> DisplayInfo {
    MAIN_DISPLAY.lock_irqsave().info()
}

/// Flushes the framebuffer, i.e. show on the screen.
pub fn framebuffer_flush() -> bool {
    MAIN_DISPLAY.lock_irqsave().flush().is_ok()
}

/// Returns the resolved main display IRQ, if the runtime provided one.
pub fn framebuffer_irq_id() -> Option<irq_framework::IrqId> {
    MAIN_DISPLAY.lock_irqsave().irq_id()
}

/// Enables IRQ handling in the main display driver.
pub fn framebuffer_enable_irq() {
    MAIN_DISPLAY.lock_irqsave().enable_irq();
}

/// Disables IRQ handling in the main display driver.
pub fn framebuffer_disable_irq() {
    MAIN_DISPLAY.lock_irqsave().disable_irq();
}

/// Acknowledges the main display IRQ source.
pub fn framebuffer_handle_irq() -> bool {
    let mut display = MAIN_DISPLAY.lock_irqsave();
    display.is_irq_enabled() && display.handle_irq()
}

// --- 3D API ---

/// Checks if the display device supports virgl 3D.
pub fn has_virgl() -> bool {
    lock_display().has_virgl()
}

/// Checks if `VIRTIO_GPU_F_RESOURCE_BLOB` was negotiated (blob resources /
/// dma-buf sharing).
pub fn has_resource_blob() -> bool {
    lock_display().has_resource_blob()
}

/// Create a 3D rendering context.
pub fn gpu3d_ctx_create(ctx_id: u32, name: &str, context_init: u32) -> DisplayResult {
    lock_display().ctx_create(ctx_id, name, context_init)
}

/// Destroy a 3D rendering context.
pub fn gpu3d_ctx_destroy(ctx_id: u32) -> DisplayResult {
    lock_display().ctx_destroy(ctx_id)
}

/// Attach a 3D resource to a rendering context.
pub fn gpu3d_ctx_attach_resource(ctx_id: u32, resource_id: u32) -> DisplayResult {
    lock_display().ctx_attach_resource(ctx_id, resource_id)
}

/// Detach a 3D resource from a rendering context.
pub fn gpu3d_ctx_detach_resource(ctx_id: u32, resource_id: u32) -> DisplayResult {
    lock_display().ctx_detach_resource(ctx_id, resource_id)
}

// --- 2D resource / scanout forwarding ---

/// Create a 2D resource on the host (for dumb buffer backing).
pub fn gpu3d_resource_create_2d(resource_id: u32, width: u32, height: u32) -> DisplayResult {
    lock_display().resource_create_2d(resource_id, width, height)
}

/// Attach guest memory backing to a resource.
pub fn gpu3d_attach_backing(resource_id: u32, paddr: u64, length: u32) -> DisplayResult {
    lock_display().resource_attach_backing(resource_id, paddr, length)
}

/// Bind a resource as the display output (scanout) for a given scanout ID.
///
/// Maps to `VIRTIO_GPU_CMD_SET_SCANOUT`. For zero-copy display: after
/// rendering into a resource, call this to bind it as the scanout.
pub fn gpu3d_set_scanout(
    scanout_id: u32,
    resource_id: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> DisplayResult {
    lock_display().set_scanout(scanout_id, resource_id, x, y, w, h)
}

/// Transfer a rectangular region of a 2D resource from guest to host.
///
/// Maps to `VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D`. Makes the host aware
/// of guest-written pixel data before a [`gpu3d_resource_flush`].
pub fn gpu3d_transfer_to_host_2d(
    resource_id: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> DisplayResult {
    lock_display().transfer_to_host_2d(resource_id, x, y, w, h)
}

/// Flush a resource's contents to the display.
///
/// Maps to `VIRTIO_GPU_CMD_RESOURCE_FLUSH`. After rendering and
/// optionally binding with [`gpu3d_set_scanout`], call this to make
/// the host display the contents.
pub fn gpu3d_resource_flush(resource_id: u32, x: u32, y: u32, w: u32, h: u32) -> DisplayResult {
    lock_display().resource_flush(resource_id, x, y, w, h)
}

/// Create a 3D resource.
///
/// The caller must explicitly call [`gpu3d_ctx_attach_resource`] after creation
/// before using the resource in rendering commands.
#[allow(clippy::too_many_arguments)] // RDIF/DRM wire signature, fixed by the protocol
pub fn gpu3d_resource_create(
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
) -> DisplayResult {
    lock_display().resource_create_3d(
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
    )
}

/// Unreference a 3D resource.
pub fn gpu3d_resource_unref(resource_id: u32) -> DisplayResult {
    lock_display().resource_unref(resource_id)
}

/// Create a blob resource (host-visible memory / dma-buf sharing).
///
/// `blob_mem` is `VIRTIO_GPU_BLOB_MEM_*`, `blob_flags` is the
/// `VIRTIO_GPU_BLOB_FLAG_*` set. `cmd` is the optional virgl command stream
/// for the blob's initial state (submitted before RESOURCE_CREATE_BLOB,
/// matching Linux ordering).
pub fn gpu3d_resource_create_blob(
    ctx_id: u32,
    resource_id: u32,
    blob_mem: u32,
    blob_flags: u32,
    size: u64,
    blob_id: u64,
    cmd: &[u8],
) -> DisplayResult {
    lock_display().resource_create_blob(
        ctx_id,
        resource_id,
        blob_mem,
        blob_flags,
        size,
        blob_id,
        cmd,
    )
}

/// Transfer data from guest to host for a 3D resource.
pub fn gpu3d_transfer_to_host(
    ctx_id: u32,
    resource_id: u32,
    box_: TransferBox,
    offset: u64,
    level: u32,
    stride: u32,
    layer_stride: u32,
) -> DisplayResult {
    lock_display().transfer_to_host_3d(
        ctx_id,
        resource_id,
        box_,
        offset,
        level,
        stride,
        layer_stride,
    )
}

/// Transfer data from host to guest for a 3D resource.
pub fn gpu3d_transfer_from_host(
    ctx_id: u32,
    resource_id: u32,
    box_: TransferBox,
    offset: u64,
    level: u32,
    stride: u32,
    layer_stride: u32,
) -> DisplayResult {
    lock_display().transfer_from_host_3d(
        ctx_id,
        resource_id,
        box_,
        offset,
        level,
        stride,
        layer_stride,
    )
}

/// Submit a virgl command buffer. Returns a monotonically increasing fence ID.
pub fn gpu3d_submit_cmd(ctx_id: u32, cmds: &[u8]) -> Result<u64, DisplayError> {
    lock_display().submit_cmd(ctx_id, cmds)
}

/// Block until the submit identified by `fence_id` has completed on the host —
/// the honest completion signal behind VIRTGPU_WAIT (Linux
/// `virtio_gpu_wait_ioctl` → `dma_resv_wait_timeout`).
pub fn gpu3d_wait_fence(fence_id: u64) -> Result<(), DisplayError> {
    lock_display().wait_fence(fence_id)
}

/// Non-blocking fence query — Linux `dma_resv_test_signaled` (the NOWAIT probe
/// in `virtio_gpu_wait_ioctl`). `false` means the host is still busy with the
/// batch.
pub fn gpu3d_fence_completed(fence_id: u64) -> Result<bool, DisplayError> {
    lock_display().fence_completed(fence_id)
}

/// Query capset information by index.
pub fn gpu3d_capset_info(index: u32) -> Result<CapsetInfo, DisplayError> {
    lock_display().capset_info(index)
}

/// Retrieve capset data.
pub fn gpu3d_capset(id: u32, ver: u32, size: u32) -> Result<alloc::vec::Vec<u8>, DisplayError> {
    lock_display().capset(id, ver, size)
}

/// Flush any pending fire-and-forget control-queue commands and notify the
/// host — an ioctl/transaction boundary (Linux `virtio_gpu_notify()`, vq.c:551).
///
/// Call exactly once at the end of an ioctl that enqueued commands whose
/// response the caller does not wait for, so the whole batch is delivered to
/// the host with a single kick. No-op when nothing is pending and when no
/// display device is initialized (no commands could have been enqueued).
pub fn gpu3d_ctrl_notify() {
    if !MAIN_DISPLAY.is_inited() {
        return;
    }
    lock_display().ctrl_notify();
}

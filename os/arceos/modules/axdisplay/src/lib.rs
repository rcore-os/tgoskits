//! [ArceOS](https://github.com/arceos-org/arceos) display module.
//!
//! Currently only supports direct writing to the framebuffer.

#![no_std]

extern crate alloc;

mod device;
pub mod rdif;
mod types;

use ax_lazyinit::LazyInit;
use ax_task::sync::RawSpinLock as Mutex;
pub use device::{DisplayDevice, DisplayError, DisplayResult, ErasedDisplayDevice, Gpu3dErrorKind};
pub use types::{
    BlobMemory, CapsetInfo, DisplayInfo, PixelFormat, ResourceCreate3d, ResourceCreateBlob,
    Transfer3d, TransferBox,
};

static MAIN_DISPLAY: LazyInit<Mutex<ErasedDisplayDevice>> = LazyInit::new();

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

/// Restore the driver's own framebuffer as the active scanout.
pub fn framebuffer_restore_scanout() -> DisplayResult {
    MAIN_DISPLAY.lock_irqsave().restore_framebuffer_scanout()
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
//
// Every entry point below runs in task context. Hot-path commands (resource
// create/attach/transfer/flush, submit) are fire-and-forget in the device
// backend: they enqueue on the control queue and return before the host has
// applied them, so callers must keep the batching contract (one
// `gpu3d_ctrl_notify()` per transaction) and observe completion through the
// fence API (`gpu3d_wait_fence` / `gpu3d_fence_completed`). The display IRQ
// path (`framebuffer_handle_irq`) takes the same `MAIN_DISPLAY` lock, so a
// plain `lock()` here would let a display IRQ interrupt the critical section
// on the same CPU and spin forever on the lock we still hold. These
// forwarding paths therefore use `lock_irqsave()`, the same discipline as the
// IRQ-safe `framebuffer_*` helpers above.

/// Checks if the display device supports virgl 3D.
pub fn has_virgl() -> bool {
    MAIN_DISPLAY.lock_irqsave().has_virgl()
}

/// Checks if `VIRTIO_GPU_F_RESOURCE_BLOB` was negotiated (blob resources /
/// dma-buf sharing).
pub fn has_resource_blob() -> bool {
    MAIN_DISPLAY.lock_irqsave().has_resource_blob()
}

/// Checks if `VIRTIO_GPU_F_CONTEXT_INIT` was negotiated.
pub fn has_context_init() -> bool {
    MAIN_DISPLAY.lock_irqsave().has_context_init()
}

/// Create a 3D rendering context.
pub fn gpu3d_ctx_create(ctx_id: u32, name: &str, context_init: u32) -> DisplayResult {
    MAIN_DISPLAY
        .lock_irqsave()
        .ctx_create(ctx_id, name, context_init)
}

/// Destroy a 3D rendering context.
pub fn gpu3d_ctx_destroy(ctx_id: u32) -> DisplayResult {
    MAIN_DISPLAY.lock_irqsave().ctx_destroy(ctx_id)
}

/// Attach a 3D resource to a rendering context.
pub fn gpu3d_ctx_attach_resource(ctx_id: u32, resource_id: u32) -> DisplayResult {
    MAIN_DISPLAY
        .lock_irqsave()
        .ctx_attach_resource(ctx_id, resource_id)
}

/// Detach a 3D resource from a rendering context.
pub fn gpu3d_ctx_detach_resource(ctx_id: u32, resource_id: u32) -> DisplayResult {
    MAIN_DISPLAY
        .lock_irqsave()
        .ctx_detach_resource(ctx_id, resource_id)
}

// --- 2D resource / scanout forwarding ---

/// Create a 2D resource on the host (for dumb buffer backing).
pub fn gpu3d_resource_create_2d(resource_id: u32, width: u32, height: u32) -> DisplayResult {
    MAIN_DISPLAY
        .lock_irqsave()
        .resource_create_2d(resource_id, width, height)
}

/// Attach guest memory backing to a resource.
pub fn gpu3d_attach_backing(resource_id: u32, paddr: u64, length: u32) -> DisplayResult {
    MAIN_DISPLAY
        .lock_irqsave()
        .resource_attach_backing(resource_id, paddr, length)
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
    MAIN_DISPLAY
        .lock_irqsave()
        .set_scanout(scanout_id, resource_id, x, y, w, h)
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
    MAIN_DISPLAY
        .lock_irqsave()
        .transfer_to_host_2d(resource_id, x, y, w, h)
}

/// Flush a resource's contents to the display.
///
/// Maps to `VIRTIO_GPU_CMD_RESOURCE_FLUSH`. After rendering and
/// optionally binding with [`gpu3d_set_scanout`], call this to make
/// the host display the contents.
pub fn gpu3d_resource_flush(resource_id: u32, x: u32, y: u32, w: u32, h: u32) -> DisplayResult {
    MAIN_DISPLAY
        .lock_irqsave()
        .resource_flush(resource_id, x, y, w, h)
}

/// Create a 3D resource.
///
/// The caller must explicitly call [`gpu3d_ctx_attach_resource`] after creation
/// before using the resource in rendering commands.
pub fn gpu3d_resource_create(params: ResourceCreate3d) -> DisplayResult {
    MAIN_DISPLAY.lock_irqsave().resource_create_3d(params)
}

/// Unreference a 3D resource.
pub fn gpu3d_resource_unref(resource_id: u32) -> DisplayResult {
    MAIN_DISPLAY.lock_irqsave().resource_unref(resource_id)
}

/// Create a blob resource (host-visible memory / dma-buf sharing).
///
/// `blob_mem` is `VIRTIO_GPU_BLOB_MEM_*`, `blob_flags` is the
/// `VIRTIO_GPU_BLOB_FLAG_*` set. `cmd` is the optional virgl command stream
/// for the blob's initial state (submitted before RESOURCE_CREATE_BLOB,
/// matching Linux ordering).
pub fn gpu3d_resource_create_blob(params: ResourceCreateBlob<'_>) -> DisplayResult {
    MAIN_DISPLAY.lock_irqsave().resource_create_blob(params)
}

/// Transfer data from guest to host for a 3D resource.
pub fn gpu3d_transfer_to_host(params: Transfer3d) -> DisplayResult {
    MAIN_DISPLAY.lock_irqsave().transfer_to_host_3d(params)
}

/// Transfer data from host to guest for a 3D resource.
pub fn gpu3d_transfer_from_host(params: Transfer3d) -> DisplayResult {
    MAIN_DISPLAY.lock_irqsave().transfer_from_host_3d(params)
}

/// Submit a virgl command buffer. Returns a monotonically increasing fence ID.
///
/// The submit is fire-and-forget: the returned fence signals when the host has
/// finished the batch, observable via [`gpu3d_wait_fence`] or
/// [`gpu3d_fence_completed`].
pub fn gpu3d_submit_cmd(ctx_id: u32, cmds: &[u8]) -> Result<u64, DisplayError> {
    MAIN_DISPLAY.lock_irqsave().submit_cmd(ctx_id, cmds)
}

/// Block until the submit identified by `fence_id` has completed on the host —
/// the honest completion signal behind VIRTGPU_WAIT (Linux
/// `virtio_gpu_wait_ioctl` → `dma_resv_wait_timeout`).
pub fn gpu3d_wait_fence(fence_id: u64) -> Result<(), DisplayError> {
    MAIN_DISPLAY.lock_irqsave().wait_fence(fence_id)
}

/// Non-blocking fence query — Linux `dma_resv_test_signaled` (the NOWAIT probe
/// in `virtio_gpu_wait_ioctl`). `false` means the host is still busy with the
/// batch.
pub fn gpu3d_fence_completed(fence_id: u64) -> Result<bool, DisplayError> {
    MAIN_DISPLAY.lock_irqsave().fence_completed(fence_id)
}

/// Completion-level-only fence query without draining the used ring. Intended
/// for callers whose pump has already advanced the completion level.
pub fn gpu3d_fence_completed_no_pump(fence_id: u64) -> Result<bool, DisplayError> {
    MAIN_DISPLAY
        .lock_irqsave()
        .fence_completed_no_pump(fence_id)
}

/// Drain the host completion queue without blocking. Call after fire-and-forget
/// submits so the next completion triggers the device IRQ promptly (Linux's
/// virtio-gpu pumps in its completion worker after every IRQ, keeping
/// fence-signal latency at µs instead of up to a frame).
pub fn gpu3d_pump() -> Result<(), DisplayError> {
    MAIN_DISPLAY.lock_irqsave().pump()
}

/// Flush any pending fire-and-forget control-queue commands and notify the
/// host — an ioctl/transaction boundary (Linux `virtio_gpu_notify()`,
/// vq.c:551).
///
/// Call exactly once at the end of an ioctl that enqueued commands whose
/// response the caller does not wait for, so the whole batch is delivered to
/// the host with a single kick. No-op when nothing is pending and when no
/// display device is initialized (no commands could have been enqueued).
pub fn gpu3d_ctrl_notify() {
    // Card0 exists as a device node even on configs without a GPU (the
    // system test suite), so callers reach this with the lazy static never
    // initialized; locking it there would panic. Without a device nothing
    // can have been enqueued, so the notify is a no-op.
    if !MAIN_DISPLAY.is_inited() {
        return;
    }
    MAIN_DISPLAY.lock_irqsave().ctrl_notify();
}

/// Query capset information by index.
pub fn gpu3d_capset_info(index: u32) -> Result<CapsetInfo, DisplayError> {
    MAIN_DISPLAY.lock_irqsave().capset_info(index)
}

/// Retrieve capset data.
pub fn gpu3d_capset(id: u32, ver: u32, size: u32) -> Result<alloc::vec::Vec<u8>, DisplayError> {
    MAIN_DISPLAY.lock_irqsave().capset(id, ver, size)
}

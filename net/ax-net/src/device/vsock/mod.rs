//! IRQ-driven vsock device runtime and socket-facing transport access.
//!
//! Vsock is driven outside the smoltcp IP path. This module owns the single
//! registered vsock interface, adapts its event stream into the connection
//! manager, and binds its hard IRQ to one fixed-CPU task-context worker.
//!
//! # IRQ Model
//!
//! The vsock device exposes connection and credit events rather than IP
//! packets. Hard IRQ context only acknowledges/coalesces work. The fixed worker
//! drains a bounded event batch, publishes socket readiness, and closes the
//! IRQ-versus-sleep race before parking. There is no periodic fallback.
//!
//! # Isolation From IP Stack
//!
//! This code must not acquire smoltcp service/socket locks. Vsock readiness is
//! handled through its own connection manager and socket transport layer.

use alloc::{boxed::Box, string::String, vec::Vec};

use ax_lazyinit::OnceLock;
use ax_sync::MutexGuard;
use irq_framework::IrqId;
use rdif_vsock::{Interface, VsockAddr, VsockConnId, VsockError, VsockIrqEndpoints};

use crate::{NetError, NetResult, PinnedNetIrqRegistrar};

mod irq_runtime;

use irq_runtime::VsockIrqRuntime;
pub use irq_runtime::VsockRuntimeError;

pub type VsockDevice = Box<dyn Interface>;
pub type VsockDeviceList = Vec<VsockDeviceInput>;

/// One resolved vsock device and its mandatory IRQ capabilities.
pub struct VsockDeviceInput {
    pub name: String,
    pub device: VsockDevice,
    pub irq: IrqId,
    pub endpoints: VsockIrqEndpoints,
}

static VSOCK_RUNTIME: OnceLock<VsockIrqRuntime> = OnceLock::new();

/// Initializes the single IRQ-driven vsock device runtime.
pub fn init_vsock_device(
    mut devices: VsockDeviceList,
    registrar: &dyn PinnedNetIrqRegistrar,
    owner_cpu: usize,
    topology_len: usize,
) -> Result<(), VsockRuntimeError> {
    if devices.is_empty() {
        return Ok(());
    }
    if devices.len() != 1 || VSOCK_RUNTIME.get().is_some() {
        return Err(VsockRuntimeError::InvalidTopology);
    }
    let input = devices.pop().expect("one validated vsock input must exist");
    let runtime = VsockIrqRuntime::start(input, registrar, owner_cpu, topology_len)?;
    VSOCK_RUNTIME.call_once(|| runtime);
    Ok(())
}

fn device() -> NetResult<MutexGuard<'static, VsockDevice>> {
    let runtime = VSOCK_RUNTIME.get().ok_or(NetError::NotFound)?;
    Ok(runtime.device().lock())
}

pub(crate) fn request_vsock_work() {
    if let Some(runtime) = VSOCK_RUNTIME.get() {
        runtime.request_task_work();
    }
}

pub fn vsock_listen(addr: VsockAddr) -> NetResult<()> {
    device()?.listen(addr.port).map_err(map_vsock_error)
}

fn map_vsock_error(e: VsockError) -> NetError {
    match e {
        VsockError::AlreadyExists => NetError::AlreadyExists,
        VsockError::Retry => NetError::WouldBlock,
        VsockError::NotConnected => NetError::NotConnected,
        VsockError::NotAvailable => NetError::NotFound,
        VsockError::NotSupported => NetError::Unsupported,
        VsockError::Other(_) => NetError::BadState,
    }
}

pub fn vsock_connect(conn_id: VsockConnId) -> NetResult<()> {
    device()?.connect(conn_id).map_err(map_vsock_error)
}

pub fn vsock_send(conn_id: VsockConnId, buf: &[u8]) -> NetResult<usize> {
    device()?.send(conn_id, buf).map_err(map_vsock_error)
}

pub fn vsock_send_capacity(conn_id: VsockConnId) -> NetResult<usize> {
    device()?.send_capacity(conn_id).map_err(map_vsock_error)
}

pub fn vsock_disconnect(conn_id: VsockConnId) -> NetResult<()> {
    device()?.disconnect(conn_id).map_err(map_vsock_error)
}

pub fn vsock_guest_cid() -> NetResult<u64> {
    Ok(device()?.guest_cid())
}

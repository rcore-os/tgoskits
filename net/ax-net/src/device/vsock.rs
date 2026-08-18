//! Vsock device polling glue.
//!
//! Vsock is driven outside the smoltcp IP path. This module owns the single
//! registered vsock interface, adapts its event stream into the connection
//! manager, and starts an adaptive poll task while vsock connections exist.
//!
//! # Polling Model
//!
//! The vsock device exposes connection and credit events rather than IP
//! packets. A reference-counted poll task runs only while stream transports are
//! active, backs off when no events are observed, and pushes data into the
//! vsock connection manager's byte rings.
//!
//! # Isolation From IP Stack
//!
//! This code must not acquire smoltcp service/socket locks. Vsock readiness is
//! handled through its own connection manager and socket transport layer.

use ax_sync::Mutex;
use rdif_vsock::{Interface, VsockAddr, VsockConnId, VsockError};

use crate::{NetError, NetResult};

mod poll;

#[cfg(all(axtest, feature = "axtest"))]
pub(crate) use poll::run_axtest_contracts;
pub(crate) use poll::{VsockPollLease, start_vsock_poll};

pub type VsockDevice = alloc::boxed::Box<dyn Interface>;
pub type VsockDeviceList = alloc::vec::Vec<VsockDevice>;

// we need a global and static only one vsock device
static VSOCK_DEVICE: Mutex<Option<VsockDevice>> = Mutex::new(None);

/// Registers the single vsock device used by the system.
pub fn register_vsock_device(dev: VsockDevice) -> NetResult {
    let mut guard = VSOCK_DEVICE.lock();
    if guard.is_some() {
        return Err(NetError::AlreadyExists);
    }
    *guard = Some(dev);
    drop(guard);
    Ok(())
}

pub fn vsock_listen(addr: VsockAddr) -> NetResult<()> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(NetError::NotFound)?;
    dev.listen(addr.port).map_err(map_vsock_error)
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
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(NetError::NotFound)?;
    dev.connect(conn_id).map_err(map_vsock_error)
}

pub fn vsock_send(conn_id: VsockConnId, buf: &[u8]) -> NetResult<usize> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(NetError::NotFound)?;
    dev.send(conn_id, buf).map_err(map_vsock_error)
}

pub fn vsock_send_capacity(conn_id: VsockConnId) -> NetResult<usize> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(NetError::NotFound)?;
    dev.send_capacity(conn_id).map_err(map_vsock_error)
}

pub fn vsock_disconnect(conn_id: VsockConnId) -> NetResult<()> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(NetError::NotFound)?;
    dev.disconnect(conn_id).map_err(map_vsock_error)
}

pub fn vsock_guest_cid() -> NetResult<u64> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(NetError::NotFound)?;
    Ok(dev.guest_cid())
}

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

use ax_errno::{AxError, AxResult, ax_bail};
use ax_sync::PiMutex;
use rdif_vsock::{Interface, VsockAddr, VsockConnId, VsockError};

use crate::vsock::connection_manager::VSOCK_CONN_MANAGER;

mod poll;

pub(crate) use poll::{VsockPollLease, start_vsock_poll};

pub type VsockDevice = alloc::boxed::Box<dyn Interface>;
pub type VsockDeviceList = alloc::vec::Vec<VsockDevice>;

// we need a global and static only one vsock device
static VSOCK_DEVICE: PiMutex<Option<VsockDevice>> = PiMutex::new(None);

/// Registers the single vsock device used by the system.
pub fn register_vsock_device(dev: VsockDevice) -> AxResult {
    let mut guard = VSOCK_DEVICE.lock();
    if guard.is_some() {
        ax_bail!(AlreadyExists, "vsock device already registered");
    }
    *guard = Some(dev);
    drop(guard);
    Ok(())
}

pub fn vsock_listen(addr: VsockAddr) -> AxResult<()> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(AxError::NotFound)?;
    dev.listen(addr.port).map_err(map_vsock_error)
}

fn map_vsock_error(e: VsockError) -> AxError {
    match e {
        VsockError::AlreadyExists => AxError::AlreadyExists,
        VsockError::Retry => AxError::WouldBlock,
        VsockError::NotConnected => AxError::NotConnected,
        VsockError::NotAvailable => AxError::NotFound,
        VsockError::NotSupported => AxError::Unsupported,
        VsockError::Other(_) => AxError::BadState,
    }
}

pub fn vsock_connect(conn_id: VsockConnId) -> AxResult<()> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(AxError::NotFound)?;
    dev.connect(conn_id).map_err(map_vsock_error)
}

pub fn vsock_send(conn_id: VsockConnId, buf: &[u8]) -> AxResult<usize> {
    let max_retries = 10; // Tests have shown that no more than two retries will be notified
    for _ in 0..max_retries {
        let result = {
            let mut guard = VSOCK_DEVICE.lock();
            let dev = guard.as_mut().ok_or(AxError::NotFound)?;
            dev.send(conn_id, buf)
        };
        match result {
            Ok(len) => return Ok(len),
            Err(VsockError::Retry) => {
                let manager = VSOCK_CONN_MANAGER.lock();
                if let Some(conn) = manager.get_connection(conn_id) {
                    drop(manager);
                    conn.wait_for_tx();
                };
            }
            Err(e) => return Err(map_vsock_error(e)),
        }
    }
    Err(map_vsock_error(VsockError::Retry))
}

pub fn vsock_disconnect(conn_id: VsockConnId) -> AxResult<()> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(AxError::NotFound)?;
    dev.disconnect(conn_id).map_err(map_vsock_error)
}

pub fn vsock_guest_cid() -> AxResult<u64> {
    let mut guard = VSOCK_DEVICE.lock();
    let dev = guard.as_mut().ok_or(AxError::NotFound)?;
    Ok(dev.guest_cid())
}

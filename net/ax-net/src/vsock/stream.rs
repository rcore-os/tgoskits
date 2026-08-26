//! Vsock stream transport.
//!
//! Stream sockets are backed by entries in the vsock connection manager and are
//! driven by the adaptive vsock device poll task rather than the smoltcp IP
//! poller.
//!
//! # Public State
//!
//! The transport uses `StateLock` for POSIX-facing socket transitions while the
//! connection manager tracks host-visible vsock connection state. Operations
//! must keep those two views synchronized at bind, listen, connect, accept, and
//! shutdown boundaries.
//!
//! # Readiness
//!
//! Poll readiness comes from connection-manager wait queues and poll sets. The
//! stream transport must not acquire smoltcp service/socket locks because vsock
//! is independent from the IP protocol core.

use alloc::sync::Arc;

use ax_sync::Mutex;

use super::connection_manager::Connection;
use crate::{
    NetError, NetResult,
    general::GeneralOptions,
    options::{Configurable, GetSocketOption, SetSocketOption},
    state::{State, StateLock},
    vsock::VsockConnId,
};

mod ops;
mod poll;

/// Stream transport for vsock sockets.
pub struct VsockStreamTransport {
    /// Connection id registered with the vsock manager.
    conn_id: Mutex<Option<VsockConnId>>,
    /// Shared connection state once bound, connecting, or connected.
    connection: Mutex<Option<Arc<Connection>>>,
    /// Public POSIX-facing stream state.
    state: StateLock,
    /// Shared socket options.
    general: GeneralOptions,
}

impl VsockStreamTransport {
    /// Create a new idle vsock stream transport.
    pub fn new() -> Self {
        Self {
            conn_id: Mutex::new(None),
            connection: Mutex::new(None),
            state: StateLock::new(State::Idle),
            general: GeneralOptions::new(1, 40, 0), // SOCK_STREAM
        }
    }

    /// Returns the manager connection associated with this stream.
    fn get_connection(&self) -> NetResult<Arc<Connection>> {
        self.connection.lock().clone().ok_or(NetError::NotConnected)
    }
}

impl Default for VsockStreamTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Configurable for VsockStreamTransport {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> NetResult<bool> {
        self.general.get_option_inner(opt)
    }

    fn set_option_inner(&self, opt: SetSocketOption) -> NetResult<bool> {
        self.general.set_option_inner(opt)
    }
}

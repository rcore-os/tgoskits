//! ARM SCMI (System Control and Management Interface) protocol implementation.
//!
//! This crate provides a `no_std` Rust interface for communicating with an
//! SCMI platform (secure monitor or SCP) over pluggable transports.
//!
//! # Overview
//!
//! - [`Scmi`] is the top-level entry point. It wraps a [`Transport`] and a
//!   shared-memory window ([`Shmem`]).
//! - Protocol clients (e.g. [`Clock`]) are obtained from the [`Scmi`] instance
//!   and expose high-level operations.
//! - A low-level [`Xfer`] / [`FuturePoll`] mechanism is available for
//!   non-blocking or custom protocol implementations.
//!
//! # Quick start
//!
//! ```ignore
//! use arm_scmi_rs::{Scmi, Smc, Shmem};
//!
//! let smc  = Smc::new(0x82000010, None);
//! let shmem = unsafe { Shmem::new(addr, bus_addr, size) };
//! let scmi = Scmi::new(smc, shmem);
//!
//! let mut clk = scmi.protocol_clk();
//! clk.clk_enable(0)?;
//! let rate = clk.rate_get(0)?;
//! ```
//!
//! [`Clock`]: protocol::Clock

#![no_std]

#[macro_use]
extern crate alloc;
#[macro_use]
extern crate log;

pub use crate::{
    err::ScmiError,
    protocol::{Base, Clock, Xfer},
    shmem::Shmem,
};

mod err;
mod protocol;
mod shmem;
mod transport;

use alloc::sync::Arc;

use spin::Mutex;
pub use transport::{Smc, Transport};

type Data<T> = Arc<Mutex<ScmiData<T>>>;

const SCMI_CLOCK_PROTOCOL: u32 = 0x14;
const SCMI_CLOCK_RATE_SET: u8 = 0x05;
const SCMI_CLOCK_RATE_GET: u8 = 0x06;
const SHMEM_PAYLOAD_OFFSET: usize = 0x1c;

/// Top-level SCMI agent handle.
///
/// Generic over the transport layer `T` so that the same protocol logic works
/// with SMC, mailbox, or any other transport that implements [`Transport`].
pub struct Scmi<T: Transport> {
    data: Data<T>,
}

impl<T: Transport> Scmi<T> {
    /// Create a new SCMI agent.
    ///
    /// Resets the shared-memory window and stores the transport for
    /// subsequent protocol calls.
    pub fn new(kind: T, mut shmem: Shmem) -> Self {
        shmem.reset();
        let data = ScmiData {
            transport: kind,
            shmem,
        };
        Scmi {
            data: Arc::new(Mutex::new(data)),
        }
    }

    /// Obtain a Base protocol client.
    ///
    /// The Base protocol is mandatory and provides vendor/protocol discovery.
    pub fn protocol_base(&self) -> protocol::Base<T> {
        protocol::Base::new(protocol::Protocol::new(
            self.data.clone(),
            protocol::Base::<T>::PROTOCOL_ID,
        ))
    }

    /// Obtain an initialised Clock protocol client.
    ///
    /// Queries the platform for the protocol version and attributes before
    /// returning the client.
    pub fn protocol_clk(&self) -> protocol::Clock<T> {
        let data = self.data.clone();
        let mut clk = protocol::Clock::new(protocol::Protocol::new(
            data,
            protocol::Clock::<T>::PROTOCOL_ID,
        ));
        clk.init();
        clk
    }

    /// Obtain a Clock protocol client **without** querying version/attributes.
    ///
    /// Useful when the caller wants to control initialisation, or when the
    /// platform is already known to support the protocol.
    pub fn protocol_clk_no_init(&self) -> protocol::Clock<T> {
        protocol::Clock::new(protocol::Protocol::new(
            self.data.clone(),
            protocol::Clock::<T>::PROTOCOL_ID,
        ))
    }
}

impl Scmi<Smc> {
    /// Get a clock rate by writing directly to shared memory (bypasses Xfer).
    ///
    /// This is a fast-path that avoids the `Xfer` / `FuturePoll` machinery
    /// entirely, suitable for synchronous SMC transports.
    pub fn clock_rate_get_direct(&self, clock_id: u32) -> Result<u64, ScmiError> {
        let mut data = self.data.lock();
        if data.shmem.size < SHMEM_PAYLOAD_OFFSET + 12 {
            return Err(ScmiError::ProtocolError);
        }
        data.shmem
            .write_message_header(SCMI_CLOCK_PROTOCOL, SCMI_CLOCK_RATE_GET, 4)?;
        data.shmem.write_payload_u32(0, clock_id)?;
        data.transport.call_sync();
        let status = data.shmem.read_payload_i32(0)?;
        if let Err(err) = ScmiError::from_status(status) {
            data.shmem.reset();
            return Err(err);
        }
        let low = data.shmem.read_payload_u32(4)? as u64;
        let high = data.shmem.read_payload_u32(8)? as u64;
        data.shmem.reset();
        Ok(low | (high << 32))
    }

    /// Set a clock rate by writing directly to shared memory (bypasses Xfer).
    pub fn clock_rate_set_direct(&self, clock_id: u32, rate: u64) -> Result<(), ScmiError> {
        let mut data = self.data.lock();
        if data.shmem.size < SHMEM_PAYLOAD_OFFSET + 16 {
            return Err(ScmiError::ProtocolError);
        }
        data.shmem
            .write_message_header(SCMI_CLOCK_PROTOCOL, SCMI_CLOCK_RATE_SET, 16)?;
        data.shmem.write_payload_u32(0, 0)?;
        data.shmem.write_payload_u32(4, clock_id)?;
        data.shmem.write_payload_u32(8, rate as u32)?;
        data.shmem.write_payload_u32(12, (rate >> 32) as u32)?;
        data.transport.call_sync();
        let status = data.shmem.read_payload_i32(0)?;
        let result = ScmiError::from_status(status);
        data.shmem.reset();
        result
    }
}

struct ScmiData<T: Transport> {
    transport: T,
    shmem: Shmem,
}

impl<T: Transport> ScmiData<T> {
    pub fn send_message(&mut self, xfer: &mut Xfer) -> Result<(), crate::err::ScmiError> {
        self.transport.send_message(&mut self.shmem, xfer)
    }

    pub fn fetch_response(&mut self, xfer: &mut Xfer) -> Result<(), crate::err::ScmiError> {
        self.transport.fetch_response(&mut self.shmem, xfer)
    }
}

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
    shmem::{MSG_HEADER_WORD, PAYLOAD_WORD, Shmem},
};

mod err;
mod protocol;
mod shmem;
mod transport;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use ax_sync::RawSpinLock;
pub use transport::{Smc, Transport};

type Data<T> = Arc<RawSpinLock<ScmiData<T>>>;

/// Upper bound of the completion poll: the platform normally answers within
/// microseconds, and waiting must never block a vCPU indefinitely.
const CHANNEL_FREE_SPINS: u32 = 200_000;

/// The same bound for a relayed request, which is shorter because it runs
/// inside the guest's trap: the platform has already answered the original
/// call by the time the relay returns.
const RELAY_CHANNEL_SPINS: u32 = 20_000;

/// Set once the platform has been observed to leave the channel free at the
/// end of a request, so the warning is not repeated per transaction.
static COMPLETION_NOT_SIGNALLED: AtomicBool = AtomicBool::new(false);

/// Waits for the platform's completion signal and records its absence.
///
/// A missing signal is not an error: the SMC transport completes inside the
/// call itself, and the `SMCCC` based platforms in the field keep the channel
/// free rather than publishing `CHANNEL_FREE` per request. Waiting for the bit
/// still matters on a platform that does publish it, so both cases are covered
/// by reading the response either way.
fn await_completion(shmem: &Shmem, spins: u32) {
    if !shmem.wait_channel_free(spins) && !COMPLETION_NOT_SIGNALLED.swap(true, Ordering::Relaxed) {
        warn!(
            "SCMI channel stayed busy after {spins} spins; reading the response anyway. This \
             platform does not publish completion, which is allowed for the SMC transport."
        );
    }
}

const SCMI_CLOCK_PROTOCOL: u32 = 0x14;
/// Clock protocol message IDs, exactly as numbered by the SCMI specification
/// and the Linux client (`drivers/firmware/arm_scmi/clock.c`): `RATE_SET` is
/// 0x5, `RATE_GET` is 0x6 and `CONFIG_SET` is 0x7. The platform answers a
/// request that names the wrong message with a protocol error, which is
/// indistinguishable from a request it never received unless the message
/// contents are already known to be correct.
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
            data: Arc::new(RawSpinLock::new(data)),
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
    /// Runs one SMC call exactly as the caller issued it and returns the
    /// monitor's reply registers.
    ///
    /// This is the relay primitive: when another agent (a guest) has already
    /// written its message into the shared channel, the host forwards the call
    /// unchanged and leaves the response in place for that agent to read. It
    /// also waits for the platform to report completion, so the caller's own
    /// polling finds the channel already free.
    pub fn relay_smc(&self, x0: u64, x1: u64, x2: u64, x3: u64) -> (u64, u64, u64, u64) {
        let registers = crate::transport::smc::smc_call_raw(x0, x1, x2, x3);
        let data = unsafe { self.data.lock_raw() };
        await_completion(&data.shmem, RELAY_CHANNEL_SPINS);
        registers
    }

    /// Answers a relayed request here instead of running it on the platform.
    ///
    /// Writes `status` into the response payload and marks the channel free,
    /// because the requesting agent polls for completion before it reads.
    pub fn publish_response(&self, status: u32) {
        let mut data = unsafe { self.data.lock_raw() };
        let _ = data.shmem.write_payload_u32(0, status);
        data.shmem.mark_channel_free();
    }

    /// Reads the raw shared-memory words of the channel without issuing a
    /// request, so a message another agent wrote can be inspected.
    pub fn debug_words(&self) -> [u32; 9] {
        let data = unsafe { self.data.lock_raw() };
        let mut out = [0u32; 9];
        for (index, slot) in out.iter_mut().enumerate() {
            *slot = data.shmem.word(index * core::mem::size_of::<u32>());
        }
        out
    }

    /// Writes the status word of the response area.
    ///
    /// Used when this agent answers a relayed request itself instead of
    /// running it on the platform: the caller reads the status from the first
    /// payload word of the response.
    pub fn write_status(&self, status: u32) {
        let mut data = unsafe { self.data.lock_raw() };
        let _ = data.shmem.write_payload_u32(0, status);
    }

    /// Get a clock rate by writing directly to shared memory (bypasses Xfer).
    ///
    /// This is a fast-path that avoids the `Xfer` / `FuturePoll` machinery
    /// entirely, suitable for synchronous SMC transports.
    pub fn clock_rate_get_direct(&self, clock_id: u32) -> Result<u64, ScmiError> {
        // SAFETY: the transport serializes SCMI transactions and excludes
        // same-context re-entry while shared memory is owned by this request.
        let mut data = unsafe { self.data.lock_raw() };
        if data.shmem.size < SHMEM_PAYLOAD_OFFSET + 12 {
            return Err(ScmiError::ProtocolError);
        }
        data.shmem
            .write_message_header(SCMI_CLOCK_PROTOCOL, SCMI_CLOCK_RATE_GET, 4)?;
        data.shmem.write_payload_u32(0, clock_id)?;
        // The SMC transport polls: the platform completes before returning.
        data.shmem.set_completion_interrupt(false);
        data.transport.call_sync();
        await_completion(&data.shmem, CHANNEL_FREE_SPINS);
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
        // SAFETY: see the transaction acquisition above.
        let mut data = unsafe { self.data.lock_raw() };
        if data.shmem.size < SHMEM_PAYLOAD_OFFSET + 16 {
            return Err(ScmiError::ProtocolError);
        }
        data.shmem
            .write_message_header(SCMI_CLOCK_PROTOCOL, SCMI_CLOCK_RATE_SET, 16)?;
        data.shmem.write_payload_u32(0, 0)?;
        data.shmem.write_payload_u32(4, clock_id)?;
        data.shmem.write_payload_u32(8, rate as u32)?;
        data.shmem.write_payload_u32(12, (rate >> 32) as u32)?;
        data.shmem.set_completion_interrupt(false);
        data.transport.call_sync();
        await_completion(&data.shmem, CHANNEL_FREE_SPINS);
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

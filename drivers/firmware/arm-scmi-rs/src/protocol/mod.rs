use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, Ordering};

use mbarrier::smp_mb;
use spin::Mutex;

use crate::{Data, Transport, err::ScmiError};

pub mod base;
pub mod clock;

pub use base::Base;
pub use clock::Clock;

const PROTOCOL_VERSION: u8 = 0;
#[allow(dead_code)]
const PROTOCOL_ATTRIBUTES: u8 = 0x1;
#[allow(dead_code)]
const PROTOCOL_MESSAGE_ATTRIBUTES: u8 = 0x2;

/// A generic SCMI protocol handle bound to a specific transport.
///
/// Holds a reference-counted handle to the shared transport data and the
/// 8-bit protocol identifier used in every message header.
pub struct Protocol<T: Transport> {
    data: Data<T>,
    id: u8,
}

impl<T: Transport> Protocol<T> {
    pub(super) fn new(data: Data<T>, id: u8) -> Self {
        Self { data, id }
    }

    /// Start a transfer for this protocol.
    ///
    /// The returned [`XferFuture`] follows a three-stage poll cycle
    /// (Init → SendOk → RespOk). Callers typically drive it to completion
    /// with `nb::block!`.
    pub fn do_xfer<'a, R, F>(
        &'a mut self,
        mut xfer: Xfer,
        on_completed: F,
    ) -> XferFuture<'a, T, R, F>
    where
        F: Fn(&mut Xfer) -> Result<R, ScmiError>,
    {
        xfer.hdr.protocol_id = self.id;
        xfer.hdr.clear_status();
        xfer.status = XferStatus::Init;

        smp_mb();
        XferFuture {
            protocol: self,
            xfer,
            on_complete: on_completed,
        }
    }

    /// Query the protocol version (message ID 0x0).
    ///
    /// Returns `(major, minor)` as defined by the SCMI specification.
    pub fn version(&mut self) -> impl FuturePoll<Output = (u16, u16)> + '_ {
        let xfer = Xfer::new(PROTOCOL_VERSION, 4);
        self.do_xfer(xfer, |xfer| {
            let version = u32::from_le_bytes([xfer.rx[0], xfer.rx[1], xfer.rx[2], xfer.rx[3]]);
            let major = (version >> 16) as u16;
            let minor = (version & 0xFFFF) as u16;
            Ok((major, minor))
        })
    }

    /// Query protocol attributes (message ID 0x1).
    ///
    /// Returns the raw 32-bit attributes value. Each protocol interprets
    /// this field differently.
    #[allow(dead_code)]
    pub fn attributes(&mut self) -> impl FuturePoll<Output = u32> + '_ {
        let xfer = Xfer::new(PROTOCOL_ATTRIBUTES, 4);
        self.do_xfer(xfer, |xfer| {
            Ok(u32::from_le_bytes([
                xfer.rx[0], xfer.rx[1], xfer.rx[2], xfer.rx[3],
            ]))
        })
    }

    /// Query whether a specific message ID is supported (message ID 0x2).
    ///
    /// Returns `true` if the message is implemented by the platform.
    #[allow(dead_code)]
    pub fn message_attributes(&mut self, message_id: u8) -> impl FuturePoll<Output = bool> + '_ {
        let mut xfer = Xfer::new(PROTOCOL_MESSAGE_ATTRIBUTES, 4);
        xfer.tx
            .extend_from_slice(&(message_id as u32).to_le_bytes());
        self.do_xfer(xfer, |_xfer| Ok(true))
    }
}

/// A polling-based future for SCMI transfers.
///
/// Unlike `std::future::Future`, this uses an explicit `poll_completion`
/// interface compatible with `nb::block!` in `no_std` environments.
pub trait FuturePoll {
    type Output;
    fn poll_completion(&mut self) -> nb::Result<Self::Output, ScmiError>;
}

/// Future returned by [`Protocol::do_xfer`].
///
/// Drives a transfer through the three stages:
/// `Init` → send → `SendOk` → fetch response → `RespOk` → callback.
pub struct XferFuture<'a, T: Transport, R, F: Fn(&mut Xfer) -> Result<R, ScmiError>> {
    protocol: &'a mut Protocol<T>,
    xfer: Xfer,
    on_complete: F,
}

impl<'a, T: Transport, R, F: Fn(&mut Xfer) -> Result<R, ScmiError>> FuturePoll
    for XferFuture<'a, T, R, F>
{
    type Output = R;

    fn poll_completion(&mut self) -> nb::Result<R, ScmiError> {
        trace!("Polling completion: xfer status={:?}", self.xfer.status);
        match self.xfer.status {
            XferStatus::Init => {
                self.protocol.data.lock().send_message(&mut self.xfer)?;
                self.xfer.status = XferStatus::SendOk;
                Err(nb::Error::WouldBlock)
            }
            XferStatus::SendOk => {
                self.protocol.data.lock().fetch_response(&mut self.xfer)?;
                self.xfer.status = XferStatus::RespOk;
                Err(nb::Error::WouldBlock)
            }
            XferStatus::RespOk => {
                let res = (self.on_complete)(&mut self.xfer)?;
                self.protocol.data.lock().shmem.reset();
                Ok(res)
            }
        }
    }
}

/// Standard SCMI protocol identifiers (DEN0056, Table 5-1).
#[allow(dead_code)]
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScmiStdProtocol {
    Base     = 0x10,
    Power    = 0x11,
    System   = 0x12,
    Perf     = 0x13,
    Clock    = 0x14,
    Sensor   = 0x15,
    Reset    = 0x16,
    Voltage  = 0x17,
    Powercap = 0x18,
}

static TRANSFER_ID_COUNTER: AtomicI32 = AtomicI32::new(0);
static TOKEN_ALLOCATOR: Mutex<TokenTable> = Mutex::new(TokenTable::new());

const fn genmask(high: u32, low: u32) -> u32 {
    if high >= 32 || low >= 32 || high < low {
        0
    } else {
        let all = u32::MAX;
        let upper = all >> (31 - high);
        let lower = all << low;
        upper & lower
    }
}

const fn mask_to_max(mask: u32) -> u32 {
    if mask == 0 {
        0
    } else {
        mask >> mask.trailing_zeros()
    }
}

const MSG_ID_MASK: u32 = genmask(7, 0);
const MSG_TYPE_MASK: u32 = genmask(9, 8);
const MSG_PROTOCOL_ID_MASK: u32 = genmask(17, 10);
const MSG_TOKEN_ID_MASK: u32 = genmask(27, 18);
const MSG_TOKEN_MAX: usize = mask_to_max(MSG_TOKEN_ID_MASK) as usize + 1;

#[inline(always)]
fn field_prep(mask: u32, value: u32) -> u32 {
    let shift = mask.trailing_zeros();
    ((value & (mask >> shift)) << shift) & mask
}

/// Message(Tx/Rx) header
///
/// - id: The identifier of the message being sent
/// - protocol_id: The identifier of the protocol used to send id message
/// - type_: The SCMI type for this message
/// - seq: The token to identify the message. When a message returns, the
///   platform returns the whole message header unmodified including the
///   token
/// - status: Status of the transfer once it's complete
/// - poll_completion: Indicate if the transfer needs to be polled for
///   completion or interrupt mode is used
#[repr(C)]
#[derive(Debug, Clone, Default)]
pub struct MsgHeader {
    pub id: u8,
    pub protocol_id: u8,
    pub type_: MsgType,
    pub seq: u16,
    pub status: u32,
    pub poll_completion: bool,
}

impl MsgHeader {
    pub fn pack(&self) -> u32 {
        field_prep(MSG_ID_MASK, self.id.into())
            | field_prep(MSG_TYPE_MASK, self.type_ as u32)
            | field_prep(MSG_TOKEN_ID_MASK, self.seq.into())
            | field_prep(MSG_PROTOCOL_ID_MASK, self.protocol_id.into())
    }

    pub fn to_result(&self) -> Result<(), ScmiError> {
        ScmiError::from_status(self.status as i32)
    }

    pub fn clear_status(&mut self) {
        self.status = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum MsgType {
    #[default]
    Command         = 0,
    DelayedResponse = 2,
    Notification    = 3,
}

/// An SCMI transfer (request/response pair).
///
/// Holds the message header, TX payload, and a pre-allocated RX buffer.
/// A unique token is assigned on creation and released on drop.
pub struct Xfer {
    pub transfer_id: i32,
    pub hdr: MsgHeader,
    pub tx: Vec<u8>,
    pub rx: Vec<u8>,
    pub pending: bool,
    pub status: XferStatus,
}

impl Xfer {
    /// Create a new transfer for message `msg_id` with `rx_size` bytes of
    /// receive buffer.
    pub fn new(msg_id: u8, rx_size: usize) -> Self {
        let transfer_id = TRANSFER_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let token = TOKEN_ALLOCATOR
            .lock()
            .alloc(transfer_id)
            .expect("Alloc token fail");

        let hdr = MsgHeader {
            id: msg_id,
            seq: token,
            ..Default::default()
        };

        let tx = Vec::with_capacity(32);
        let rx = vec![0u8; rx_size];

        Self {
            transfer_id,
            hdr,
            tx,
            rx,
            pending: false,
            status: XferStatus::SendOk,
        }
    }

    pub fn token(&self) -> u16 {
        self.hdr.seq
    }
}

impl Drop for Xfer {
    fn drop(&mut self) {
        TOKEN_ALLOCATOR.lock().release(self.hdr.seq);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XferStatus {
    #[default]
    Init   = 0,
    SendOk = 1,
    RespOk = 2,
}

const TOKENS_PER_WORD: usize = 32;
#[allow(clippy::manual_div_ceil)]
const TOKEN_TABLE_WORDS: usize = (MSG_TOKEN_MAX + TOKENS_PER_WORD - 1) / TOKENS_PER_WORD;

const fn token_table_init() -> [u32; TOKEN_TABLE_WORDS] {
    [0; TOKEN_TABLE_WORDS]
}

struct TokenTable {
    bitmap: [u32; TOKEN_TABLE_WORDS],
}

impl TokenTable {
    const fn new() -> Self {
        TokenTable {
            bitmap: token_table_init(),
        }
    }

    fn alloc(&mut self, base: i32) -> Option<u16> {
        let base = base as u16;
        if self.try_alloc(base) {
            return Some(base);
        }
        (0..MSG_TOKEN_MAX as u16).find(|&token| self.try_alloc(token))
    }

    fn try_alloc(&mut self, token: u16) -> bool {
        let word_idx = token / TOKENS_PER_WORD as u16;
        let bit_idx = token % TOKENS_PER_WORD as u16;
        let mask = 1u32 << bit_idx;
        if (self.bitmap[word_idx as usize] & mask) == 0 {
            self.bitmap[word_idx as usize] |= mask;
            true
        } else {
            false
        }
    }

    fn release(&mut self, token: u16) {
        let token = token as usize;
        if token >= MSG_TOKEN_MAX {
            return;
        }
        let word_idx = token / TOKENS_PER_WORD;
        let bit_idx = token % TOKENS_PER_WORD;
        let mask = !(1u32 << bit_idx);
        self.bitmap[word_idx] &= mask;
    }
}

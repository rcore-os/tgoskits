use alloc::vec::Vec;

use nb::block;

use super::FuturePoll;
use crate::{Transport, err::ScmiError};

/// SCMI Base protocol client (protocol ID 0x10).
///
/// The Base protocol is mandatory for every SCMI agent. It provides
/// discovery of vendor information and the list of protocols supported
/// by the platform.
pub struct Base<T: Transport> {
    protocol: super::Protocol<T>,
}

/// SCMI Base protocol message IDs.
const PROTOCOL_BASE_DISCOVER_VENDOR: u8 = 0x03;
const PROTOCOL_BASE_DISCOVER_SUB_VENDOR: u8 = 0x04;
const PROTOCOL_BASE_DISCOVER_IMPLEMENTATION_VERSION: u8 = 0x05;
const PROTOCOL_BASE_DISCOVER_LIST_PROTOCOLS: u8 = 0x06;

impl<T: Transport> Base<T> {
    pub const PROTOCOL_ID: u8 = 0x10;

    pub(crate) fn new(protocol: super::Protocol<T>) -> Self {
        Self { protocol }
    }

    /// Query the vendor identifier string.
    ///
    /// Returns up to 16 bytes of ASCII text identifying the vendor.
    pub fn discover_vendor(&mut self) -> Result<Vec<u8>, ScmiError> {
        let xfer = super::Xfer::new(PROTOCOL_BASE_DISCOVER_VENDOR, 16);
        let mut fut = self.protocol.do_xfer(xfer, |xfer| {
            let mut v = Vec::with_capacity(16);
            v.extend_from_slice(&xfer.rx[..16]);
            // Trim trailing NUL bytes
            while v.last() == Some(&0) {
                v.pop();
            }
            Ok(v)
        });
        block!(fut.poll_completion())
    }

    /// Query the sub-vendor identifier string.
    pub fn discover_sub_vendor(&mut self) -> Result<Vec<u8>, ScmiError> {
        let xfer = super::Xfer::new(PROTOCOL_BASE_DISCOVER_SUB_VENDOR, 16);
        let mut fut = self.protocol.do_xfer(xfer, |xfer| {
            let mut v = Vec::with_capacity(16);
            v.extend_from_slice(&xfer.rx[..16]);
            while v.last() == Some(&0) {
                v.pop();
            }
            Ok(v)
        });
        block!(fut.poll_completion())
    }

    /// Query the implementation version (32-bit).
    pub fn discover_implementation_version(&mut self) -> Result<u32, ScmiError> {
        let xfer = super::Xfer::new(PROTOCOL_BASE_DISCOVER_IMPLEMENTATION_VERSION, 4);
        let mut fut = self.protocol.do_xfer(xfer, |xfer| {
            Ok(u32::from_le_bytes([
                xfer.rx[0], xfer.rx[1], xfer.rx[2], xfer.rx[3],
            ]))
        });
        block!(fut.poll_completion())
    }

    /// Discover the list of protocols implemented by the platform.
    ///
    /// Returns protocol IDs as a `Vec<u8>`. The skip parameter allows
    /// iterating when the platform reports more protocols than fit in
    /// one response.
    pub fn discover_list_protocols(&mut self, skip: u32) -> Result<Vec<u8>, ScmiError> {
        let mut xfer = super::Xfer::new(PROTOCOL_BASE_DISCOVER_LIST_PROTOCOLS, 20);
        xfer.tx.extend_from_slice(&skip.to_le_bytes());
        let mut fut = self.protocol.do_xfer(xfer, |xfer| {
            // First 4 bytes: number of remaining protocols (skip for now)
            let num = xfer.rx.len().saturating_sub(4);
            Ok(xfer.rx[4..4 + num].to_vec())
        });
        block!(fut.poll_completion())
    }
}

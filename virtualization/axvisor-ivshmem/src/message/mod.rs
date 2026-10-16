//! Message V2 framing for ivshmem_socket.
//!
//! Adapted from axivc's message codec. Frame decoding checks individual frame
//! structure only. The checksum accumulator covers message metadata and body;
//! callers must feed the complete body in order and compare the result.
//! Session routing, CONTROL opcodes, reservation matching, message ordering
//! and UTF-8 require the connection/message layer.
//! This module does not change or initialize the ring's section version.

mod checksum;
mod frame;

pub use checksum::MessageChecksum;
pub use frame::{DecodedFrame, FrameError, FrameKind, FrameSpec};

/// Bytes in the explicitly little-endian Message V2 header.
pub const HEADER_SIZE: usize = 44;
/// Maximum body fragment carried in one 240-byte slot payload.
pub const FRAGMENT_SIZE: usize = crate::PAYLOAD_SIZE - HEADER_SIZE;

//! SG200x JPEG Processing Unit driver.
//!
//! The driver owns its streaming DMA buffers, programs caller-mapped MMIO,
//! and exposes checked planar layouts for full and scaled baseline JPEG
//! decoding. A timeout permanently poisons the decoder and quarantines all
//! buffers that the device may still own.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]

mod decoder;
mod engine;
mod header;
mod layout;
mod regs;

pub use decoder::{DecodeResult, JpuCreateError, JpuDecodeError, JpuDecoder, JpuMmio};
pub use layout::{Extent, FrameLayout, FrameLayoutError, JpuPixelFormat, JpuScale, PlaneLayout};

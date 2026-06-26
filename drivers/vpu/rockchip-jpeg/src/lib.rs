//! Low-level device layer for the Rockchip RK3588 hardware JPEG decoder
//! (the VDPU720 / `RKDJPEG` block, DT node `jpegd@fdb90000`).
//!
//! The crate is OS-independent (`#![no_std]`) and split into:
//! - [`registers`]: the `RKDJPEG_SWREG*` register-file definitions.
//! - [`status`]: pure decoding of the `SWREG1` interrupt/status word.
//!
//! Higher layers (`command` register-array encoder, `parser` JPEG header parser,
//! and the `JpuCore` MMIO/runtime) are added as the bring-up path is verified.

#![no_std]

// The host test harness links `std`; allow tests to build JPEG fixtures with it.
#[cfg(test)]
extern crate std;

pub mod command;
pub mod parser;
pub mod registers;
pub mod status;

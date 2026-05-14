//! Runtime execution modules for QEMU, TFTP, and U-Boot.
//!
//! This module contains implementations for running operating systems
//! in various environments:
//!
//! - [`qemu`] - Running in QEMU emulator with UEFI support
//! - [`tftp`] - TFTP server for network booting
//! - [`uboot`] - U-Boot bootloader integration via serial/YMODEM

/// QEMU emulator runner with UEFI/OVMF support.
pub mod qemu;

/// TFTP server for network booting.
pub(crate) mod tftp;

/// U-Boot bootloader integration.
pub mod uboot;

/// Shared byte-stream matcher for runtime output detection.
mod output_matcher;

pub use output_matcher::{ByteStreamMatcher, StreamMatch, StreamMatchKind};

/// OVMF prebuilt firmware downloader (internal).
mod ovmf_prebuilt;

/// Shared shell auto-init matcher and delayed command sender.
pub(crate) mod shell_init;

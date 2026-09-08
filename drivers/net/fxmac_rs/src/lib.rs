//! # FXMAC Ethernet Driver
//!
//! A `no_std` Rust driver for the FXMAC Ethernet controller found on the PhytiumPi (Phytium Pi) board.
//! This driver supports DMA-based packet transmission and reception, providing a foundation for
//! network communication in embedded and bare-metal environments.
//!
//! ## Features
//!
//! - **DMA Support**: Efficient packet transmission and reception using DMA buffer descriptors.
//! - **PHY Management**: Support for PHY initialization, auto-negotiation, and manual speed configuration.
//! - **Interrupt Handling**: Built-in interrupt handlers for TX/RX completion and error conditions.
//! - **Multiple PHY Interfaces**: Support for SGMII, RGMII, RMII, XGMII, and other interface modes.
//! - **Configurable**: Supports jumbo frames, multicast filtering, and various MAC options.
//!
//! ## Target Platform
//!
//! This driver is designed for the aarch64 architecture, specifically targeting the PhytiumPi board
//! with the Motorcomm YT8521 PHY.
//!
//! ## Quick Start
//!
//! The host supplies a [`dma_api::DeviceDma`] capability for all descriptor and
//! packet-buffer allocations and an owned [`mmio_api::Mmio`] mapping for the
//! complete register aperture.
//!
//! ```ignore
//! use fxmac_rs::{xmac_init, FXmacLwipPortTx, FXmacRecvHandler};
//!
//! // Initialize the driver
//! let (mut fxmac, irq_endpoint) = xmac_init(device_dma, mmio, hardware)?;
//!
//! // Send packets
//! let mut tx_vec = Vec::new();
//! tx_vec.push(packet_data.to_vec());
//! FXmacLwipPortTx(&mut fxmac, tx_vec);
//!
//! // Receive packets
//! if let Some(recv_packets) = FXmacRecvHandler(&mut fxmac) {
//!     for packet in recv_packets {
//!         // Process received packet
//!     }
//! }
//! ```
//!
//! ## Module Structure
//!
//! - [`fxmac`]: Core MAC controller functionality and configuration.
//! - [`fxmac_dma`]: DMA buffer descriptor management and packet handling.
//! - [`fxmac_intr`]: Interrupt handling and callback management.
//! - [`fxmac_phy`]: PHY initialization and management functions.
//!
//! ## Safety and Environment
//!
//! - This crate targets `no_std` and assumes the platform provides DMA-coherent
//!   memory and interrupt routing.
//! - Most APIs interact with memory-mapped registers and should be used with
//!   care in the correct execution context.
//!
//! ## Feature Flags
//!
//! - `debug`: Enable logging via the `log` crate. Without this feature, logging
//!   macros become no-ops.

#![no_std]
#![allow(unused)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

extern crate alloc;

#[cfg(feature = "debug")]
#[macro_use]
extern crate log;

#[cfg(not(feature = "debug"))]
#[macro_use]
mod log {
    macro_rules! trace {
        ($($arg:tt)*) => {};
    }
    macro_rules! debug {
        ($($arg:tt)*) => {};
    }
    macro_rules! info {
        ($($arg:tt)*) => {};
    }
    macro_rules! warn {
        ($($arg:tt)*) => {};
    }
    macro_rules! error {
        ($($arg:tt)*) => {};
    }
}

// mod mii_const;
mod fxmac_const;

mod fxmac;
mod fxmac_dma;
mod fxmac_intr;
mod fxmac_phy;
mod utils;

// Re-exports for core MAC functionality
pub use fxmac::*;
// Re-exports for DMA operations
pub use fxmac_dma::*;
// Re-exports for PHY interface
pub use fxmac_phy::{FXmacPhyInit, FXmacPhyRead, FXmacPhyWrite};

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {}
}

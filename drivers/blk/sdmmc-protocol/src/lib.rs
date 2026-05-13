//! `no_std` SD/MMC protocol building blocks for embedded systems.
//!
//! This crate provides protocol-level types and driver skeletons for SD,
//! MMC and SDIO cards. It is transport-agnostic at the trait level and
//! brings its own SPI-mode driver plus an SDIO host-controller abstraction.
//!
//! # What you get
//!
//! - [`cmd::Command`] / [`cmd::DataDirection`]: SD/MMC command opcodes,
//!   argument encoding and per-command data direction.
//! - [`response::Response`] and friends ([`response::CidResponse`],
//!   [`response::CsdResponse`], [`response::SwitchStatus`], ...): typed
//!   parsers for the response formats defined in the SD spec.
//! - [`error::Error`]: a single error enum the drivers and parsers return.
//! - [`spi`] *(feature `spi`, on by default)*: a [`spi::SpiTransport`] trait
//!   plus the [`spi::SpiSdmmc`] driver for SPI-mode SD cards. Includes a
//!   thin [`spi::SpiDeviceWrapper`] adapter for `embedded-hal` 1.0
//!   `SpiDevice<u8>` implementations.
//! - [`sdio`] *(feature `sdio`)*: a [`sdio::SdioHost`] trait that abstracts
//!   a host controller and the [`sdio::SdioSdmmc`] driver that drives it
//!   through card initialization, block I/O and bus-speed selection.
//!
//! # Cargo features
//!
//! | Feature  | Default | Purpose                                         |
//! |----------|---------|-------------------------------------------------|
//! | `spi`    | yes     | Enables [`spi::SpiTransport`] and [`spi::SpiSdmmc`]. |
//! | `sdio`   | no      | Enables [`sdio::SdioHost`] and [`sdio::SdioSdmmc`].  |
//!
//! Diagnostic output goes through the [`log`] crate; configure a logger in
//! your application to capture it.
//!
//! # Example
//!
//! ```rust,no_run
//! use embedded_hal::delay::DelayNs;
//! use sdmmc_protocol::{
//!     Error,
//!     spi::{SpiSdmmc, SpiTransport},
//! };
//!
//! struct MySpi;
//!
//! impl SpiTransport for MySpi {
//!     fn transfer_byte(&mut self, byte: u8) -> Result<u8, Error> {
//!         # let _ = byte;
//!         # Ok(0)
//!     }
//! }
//!
//! fn boot<D: DelayNs>(spi: MySpi, delay: D) -> Result<(), Error> {
//!     let mut card = SpiSdmmc::new(spi, delay);
//!     let _info = card.init()?;
//!     let mut block = [0u8; 512];
//!     card.read_block(0, &mut block)?;
//!     Ok(())
//! }
//! ```
//!
//! # Maturity
//!
//! The SPI path has protocol-level unit tests and basic block read/write
//! support. The SDIO path is a host abstraction skeleton and needs
//! platform-specific validation before use on real hardware.
//!
//! # MSRV
//!
//! Rust 1.85 (the first stable to ship edition 2024).

#![no_std]

pub mod cmd;
mod common;
pub mod error;
pub mod ext_csd;
pub mod response;

#[cfg(feature = "spi")]
pub mod spi;

#[cfg(feature = "sdio")]
pub mod sdio;

pub use cmd::{Command, DataDirection};
pub use error::{Error, ErrorContext, Phase};
pub use response::{CidResponse, CsdResponse, Response, SwitchStatus};

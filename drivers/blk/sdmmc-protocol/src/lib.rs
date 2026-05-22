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
//! | `sdio`   | no      | Enables the host trait and submit/poll data-command contract. |
//!
//! Diagnostic output goes through the [`log`] crate; configure a logger in
//! your application to capture it.
//!
//! # Example
//!
//! ```rust,ignore
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
//! support. The SDIO path has been validated end-to-end against several host
//! controller / SoC combinations through the dedicated host backends in this
//! workspace:
//!
//! | Host crate         | SoC / controller        | Mode                  | Status |
//! |--------------------|-------------------------|-----------------------|--------|
//! | `sdhci-host`       | RK3568 (dwcmshc)        | eMMC HS@52, FIFO/DMA  | OK     |
//! | `sdhci-host`       | RK3588 (dwcmshc)        | eMMC HS@52, FIFO/DMA  | OK     |
//! | `dwmmc-host`       | RK3568 SD (dw_mshc)     | SD HS, DMA            | OK     |
//! | `phytium-mci-host` | Phytium MCI             | SD HS, DMA            | OK     |
//!
//! UHS-I / HS200 / HS400 paths exist in the state machine but have not yet
//! been signed off on a real card + IO regulator combination. See
//! `drivers/blk/sdmmc-protocol/docs/REVIEW.md` for the remaining roadmap.
//!
//! # MSRV
//!
//! Rust 1.85 (the first stable to ship edition 2024).

#![no_std]

pub mod block;
pub mod cmd;
mod common;
pub mod error;
pub mod ext_csd;
pub mod response;

#[cfg(feature = "spi")]
pub mod spi;

#[cfg(feature = "sdio")]
pub mod sdio;

pub use block::{
    BlockBufferConfig, BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode,
    BlockTransferState, CommandPoll, CommandResponsePoll, DataCommandDirection, DataCommandPoll,
    DataCommandState, OperationPoll,
};
pub use cmd::{Command, DataDirection};
pub use error::{Error, ErrorContext, Phase};
pub use response::{CidResponse, CsdResponse, Response, SwitchStatus};

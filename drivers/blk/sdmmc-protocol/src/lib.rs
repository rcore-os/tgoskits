//! `no_std` SD/MMC protocol building blocks for embedded systems.
//!
//! This crate provides protocol-level types and driver skeletons for SD,
//! MMC and SDIO cards. Native host-controller operations use explicit
//! initialization schedules and acknowledged IRQ events.
//!
//! # What you get
//!
//! - [`cmd::Command`] / [`cmd::DataDirection`]: SD/MMC command opcodes,
//!   argument encoding and per-command data direction.
//! - [`response::Response`] and friends ([`response::CidResponse`],
//!   [`response::CsdResponse`], [`response::SwitchStatus`], ...): typed
//!   parsers for the response formats defined in the SD spec.
//! - [`error::Error`]: a single error enum the drivers and parsers return.
//! - [`sdio`] *(feature `sdio`)*: a [`sdio::SdioHost`] trait that abstracts
//!   a host controller and the [`sdio::SdioSdmmc`] driver that drives it
//!   through card initialization, block I/O and bus-speed selection.
//! - [`rdif`] *(feature `rdif`)*: a [`rdif::BlockDevice`] bridge that exposes
//!   an SDIO-backed card through `rdif-block` queues.
//!
//! # Cargo features
//!
//! | Feature  | Default | Purpose                                         |
//! |----------|---------|-------------------------------------------------|
//! | `sdio` | no | Enables host traits and incremental IRQ-event data commands. |
//! | `rdif` | no | Enables the RDIF block-device bridge over `sdio`. |
//!
//! Diagnostic output goes through the [`log`] crate; configure a logger in
//! your application to capture it.
//!
//! # Maturity
//!
//! The SDIO path has been validated end-to-end against several host
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

#[cfg(any(feature = "sdio", feature = "rdif"))]
extern crate alloc;

pub mod block;
pub mod cmd;
mod common;
pub mod error;
pub mod ext_csd;
pub mod response;

#[cfg(feature = "sdio")]
pub mod sdio;

#[cfg(feature = "rdif")]
pub mod rdif;

pub use block::{
    BlockBufferConfig, BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode,
    BlockTransferState, CommandPoll, CommandResponsePoll, DataCommandDirection, DataCommandPoll,
    DataCommandState, OperationPoll,
};
pub use cmd::{Command, DataDirection};
pub use error::{Error, ErrorContext, Phase};
pub use response::{CidResponse, CsdResponse, Response, SwitchStatus};

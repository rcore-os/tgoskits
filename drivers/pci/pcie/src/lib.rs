#![no_std]

extern crate alloc;

extern crate log;

#[cfg(all(axtest, feature = "axtest"))]
pub mod axtest;

mod bar_alloc;
mod chip;
pub mod err;
mod msix;
mod root;
mod types;

pub use bar_alloc::*;
pub use chip::PcieGeneric;
pub use mmio_api::{MapError, Mmio, MmioAddr, MmioOp};
pub use msix::{MsiMessage, MsixError, MsixTableEntry, MsixTableInfo, MsixTableRegion};
pub use rdif_pcie::{Interface as Controller, PciMem32, PciMem64, PcieController};
pub use root::{
    EnumeratedEndpoint, PciIntxRoute, enumerate_by_controller, enumerate_by_controller_with_info,
};
pub use types::*;

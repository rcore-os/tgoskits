//! Allocation-free RISC-V AIA hardware operations.
//!
//! Mapping, interrupt exclusion, topology discovery and source ownership belong
//! to the caller. APLIC operations require exclusive access to the capability.
#![no_std]

mod aplic;
mod imsic;

pub use aplic::{Aplic, MsiTarget, SourceMode};
pub use imsic::FileLayout;
#[cfg(target_arch = "riscv64")]
pub use imsic::{claim_supervisor, init_supervisor};

/// Invalid hardware configuration, rejected before accessing registers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    /// The mapping is too short, unaligned, or wraps the address space.
    #[error("invalid APLIC register window")]
    InvalidWindow,
    /// A source is outside the configured APLIC domain.
    #[error("source outside configured APLIC domain")]
    InvalidSource,
    /// A hart index or external interrupt identity does not fit its field.
    #[error("MSI target does not fit its hardware fields")]
    InvalidTarget,
    /// The IMSIC layout cannot be represented by an APLIC target.
    #[error("IMSIC layout cannot be represented by APLIC")]
    InvalidLayout,
    /// The IMSIC register ranges do not describe distinct complete files.
    #[error("invalid or duplicate IMSIC file range")]
    InvalidFileRange,
    /// The register ranges cannot hold the requested interrupt files.
    #[error("insufficient register space for IMSIC files")]
    InsufficientFiles,
    /// The interrupt identity count is not a supported IMSIC bank count.
    #[error("invalid IMSIC identity count")]
    InvalidIdentityCount,
}

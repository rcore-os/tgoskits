//! Structured conventional PCI construction and access failures.

use alloc::string::String;

use super::{PciBarIndex, PciBdf};
use crate::AccessWidth;

/// Result returned by PCI topology, config, and root-state operations.
pub type PciResult<T = ()> = Result<T, PciError>;

/// A PCI address, descriptor, topology, or config access is invalid.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PciError {
    /// One numeric PCI address component is outside its architectural range.
    #[error("PCI {component} value {value:#x} is outside the supported range")]
    InvalidAddress {
        /// Address component being validated.
        component: &'static str,
        /// Rejected numeric value.
        value: u64,
    },
    /// A Type-0 identity would be interpreted as an absent function.
    #[error("invalid PCI endpoint identity: {detail}")]
    InvalidEndpointIdentity {
        /// Diagnostic reason.
        detail: &'static str,
    },
    /// A conventional config access violates width, alignment, or range rules.
    #[error("invalid PCI config access at {offset:#x} with width {width:?}: {detail}")]
    InvalidConfigAccess {
        /// Function-relative config-space offset.
        offset: u16,
        /// Requested access width.
        width: AccessWidth,
        /// Diagnostic reason.
        detail: &'static str,
    },
    /// A platform-owned config byte conflicts with core-owned state.
    #[error("invalid PCI config patch at {offset:#x}: {detail}")]
    InvalidConfigPatch {
        /// Conventional config byte offset.
        offset: u16,
        /// Rejected invariant.
        detail: &'static str,
    },
    /// The root memory aperture is malformed.
    #[error("invalid PCI host aperture: {detail}")]
    InvalidHostAperture {
        /// Diagnostic reason.
        detail: &'static str,
    },
    /// One function identity was declared more than once.
    #[error("PCI function identity {function} is declared more than once")]
    DuplicateFunction {
        /// Stable function identity.
        function: String,
    },
    /// Runtime binding named a function absent from the resolved topology.
    #[error("PCI function {function} is absent from the resolved topology")]
    UnknownFunction {
        /// Missing graph function identity.
        function: String,
    },
    /// A resolved function already has an active runtime binding.
    #[error("PCI function {function} already has an active runtime binding")]
    FunctionAlreadyBound {
        /// Already-bound graph function identity.
        function: String,
    },
    /// Two functions request the same BDF.
    #[error("PCI BDF {bdf} is requested by both {first} and {second}")]
    DuplicateBdf {
        /// Conflicting BDF.
        bdf: PciBdf,
        /// First stable function identity.
        first: String,
        /// Second stable function identity.
        second: String,
    },
    /// A fixed request targets a platform-reserved BDF.
    #[error("PCI BDF {bdf} is reserved and cannot be assigned to {function}")]
    BdfReserved {
        /// Reserved BDF.
        bdf: PciBdf,
        /// Rejected function identity.
        function: String,
    },
    /// A fixed request targets a non-zero function, which this phase does
    /// not place.
    #[error("PCI function placement {bdf} is unsupported: this phase places function 0 only")]
    UnsupportedFunctionPlacement {
        /// Rejected BDF.
        bdf: PciBdf,
    },
    /// No BDF remains on the supported root bus.
    #[error("PCI bus 0000:00 has no free function for {function}")]
    BdfExhausted {
        /// Function that could not be placed.
        function: String,
    },
    /// A BAR descriptor or placement is malformed.
    #[error("invalid PCI BAR{bar}: {detail}")]
    InvalidBar {
        /// BAR index.
        bar: PciBarIndex,
        /// Diagnostic reason.
        detail: String,
    },
    /// A BAR overlaps an already resolved BAR.
    #[error(
        "PCI BAR range [{start:#x}, {end:#x}) for {function} BAR{bar} conflicts with another BAR"
    )]
    BarConflict {
        /// Function owning the rejected BAR.
        function: String,
        /// BAR index.
        bar: PciBarIndex,
        /// Rejected range start.
        start: u64,
        /// Rejected range end.
        end: u64,
    },
    /// The PCI memory aperture cannot fit a BAR.
    #[error("PCI memory aperture cannot place {function} BAR{bar} with size {size:#x}")]
    BarApertureExhausted {
        /// Function owning the BAR.
        function: String,
        /// BAR index.
        bar: PciBarIndex,
        /// Required BAR size.
        size: u64,
    },
}

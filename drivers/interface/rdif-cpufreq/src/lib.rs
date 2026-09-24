#![no_std]

//! CPU frequency domains and operating points shared by kernel runtimes and SoC drivers.
//!
//! A domain contains CPUs that must change frequency together. The SoC driver owns
//! OPP validation, voltage and clock sequencing, read-back, and safety limits. A
//! runtime may use this interface to implement governors without knowing those
//! hardware details. Driver calls require ordinary task context and exclusive
//! access; the runtime serializes calls from governors and fixed-frequency users.

extern crate alloc;

use alloc::vec::Vec;

use rdif_base::def_driver;
pub use rdif_base::{DriverGeneric, custom_type};

custom_type!(
    #[doc = "Identifier of a hardware frequency domain within one driver."],
    DomainId, u32, "{}"
);

impl DomainId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
}

/// A hardware frequency domain and the runtime logical CPUs it controls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainInfo {
    pub id: DomainId,
    /// Logical CPU indexes used by the kernel scheduler, not FDT hardware IDs.
    pub cpu_ids: Vec<usize>,
}

/// An OPP that the driver can currently confirm and safely apply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperatingPoint {
    /// Frequency represented by this OPP in hertz.
    pub frequency_hz: u64,
    /// Required CPU supply voltage, when the driver can report it.
    pub voltage_uv: Option<u32>,
}

/// Current effective bounds for one domain, including thermal restrictions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrequencyLimits {
    pub min_hz: u64,
    pub max_hz: u64,
}

#[derive(thiserror::Error, Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrequencyError {
    #[error("CPU frequency control is unsupported")]
    NotSupported,
    #[error("CPU frequency control is not ready")]
    NotReady,
    #[error("unknown CPU frequency domain")]
    InvalidDomain,
    #[error("requested operating point is unavailable")]
    OppUnavailable,
    #[error("CPU frequency hardware operation or confirmation failed")]
    HardwareFailure,
}

/// Hardware operations for one or more CPU frequency domains.
///
/// All methods are called from sleepable task context. Implementations must
/// reject requests outside the currently available OPPs and effective limits.
/// A failed transition must not report an unconfirmed OPP as current.
pub trait Interface: DriverGeneric {
    /// Enumerates domains and their logical CPU membership.
    fn domains(&self) -> Result<Vec<DomainInfo>, FrequencyError>;

    /// Returns only OPPs currently established as safe to use on this hardware.
    fn available_opps(&self, domain: DomainId) -> Result<Vec<OperatingPoint>, FrequencyError>;

    /// Returns the last fully confirmed requested OPP.
    ///
    /// This is a policy state, not a measurement of delivered CPU frequency.
    fn current_opp(&self, domain: DomainId) -> Result<OperatingPoint, FrequencyError>;

    /// Returns the current lower and upper bounds for a domain.
    fn limits(&self, domain: DomainId) -> Result<FrequencyLimits, FrequencyError>;

    /// Applies an exact available OPP and confirms its hardware transition.
    fn set_frequency(&mut self, domain: DomainId, frequency_hz: u64) -> Result<(), FrequencyError>;

    /// Refreshes hardware-derived limits, including thermal restrictions.
    ///
    /// If a new bound excludes the current OPP, the driver must perform and
    /// confirm any required transition before reporting the new state.
    fn refresh_limits(&mut self) -> Result<(), FrequencyError>;
}

def_driver!(CpuFreq, Interface);

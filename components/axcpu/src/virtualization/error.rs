//! Hardware virtualization lifecycle errors.

/// A requested hardware ownership transition could not complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VirtualizationError {
    /// Required hardware access is not available in this execution environment.
    #[error("virtualization hardware is unavailable")]
    Unavailable,
    /// Hardware is already owned by this per-CPU object.
    #[error("virtualization is already enabled")]
    AlreadyEnabled,
    /// Hardware has not been enabled by this owner.
    #[error("virtualization is not enabled")]
    NotEnabled,
    /// None of the implemented guest paging modes are supported by this CPU.
    #[error("no implemented guest paging mode is supported")]
    UnsupportedPaging,
    /// The requested direct guest timer comparator is unavailable.
    #[error("guest timer comparator is unsupported")]
    UnsupportedTimer,
    /// Page-table root cannot be represented by the hardware root field.
    #[error("guest page-table root is misaligned or exceeds the PPN width")]
    InvalidRoot,
    /// The exception vector address violates architectural alignment.
    #[error("exception vector is misaligned")]
    InvalidVector,
    /// Hardware control memory has insufficient size or invalid alignment/range.
    #[error("invalid hardware control memory")]
    #[cfg(target_arch = "x86_64")]
    InvalidControlMemory,
    /// A virtualization instruction rejected the requested transition.
    #[error("virtualization instruction failed")]
    #[cfg(target_arch = "x86_64")]
    InstructionFailed,
    /// Firmware has not authorized and locked VMX operation.
    #[error("VMX feature control is not authorized and locked")]
    #[cfg(target_arch = "x86_64")]
    FeatureControlUnavailable,
    /// Guest extended-state mask is unsupported or violates feature dependencies.
    #[error("invalid guest extended-state configuration")]
    #[cfg(target_arch = "x86_64")]
    InvalidExtendedState,
    /// The requested I/O port range exceeds the 16-bit port namespace.
    #[error("I/O port range exceeds the hardware bitmap")]
    #[cfg(target_arch = "x86_64")]
    InvalidPortRange,
    /// The MSR number has no programmable bit in this hardware bitmap.
    #[error("MSR is not represented by the hardware permission bitmap")]
    #[cfg(target_arch = "x86_64")]
    UnsupportedMsr,
}

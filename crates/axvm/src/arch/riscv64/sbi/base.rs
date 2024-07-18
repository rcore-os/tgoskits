use axerrno::{AxError, AxResult};

/// Functions defined for the Base extension
#[derive(Clone, Copy, Debug)]
pub enum BaseFunction {
    /// Returns the implemented version of the SBI standard.
    GetSepcificationVersion,
    /// Returns the ID of the SBI implementation.
    GetImplementationID,
    /// Returns the version of the SBI implementation.
    GetImplementationVersion,
    /// Checks if the given SBI extension is supported.
    ProbeSbiExtension(u64),
    /// Returns the vendor that produced this machine(`mvendorid`).
    GetMachineVendorID,
    /// Returns the architecture implementation ID of this machine(`marchid`).
    GetMachineArchitectureID,
    /// Returns the ID of this machine(`mimpid`).
    GetMachineImplementationID,
}

impl BaseFunction {
    pub(crate) fn from_regs(args: &[usize]) -> AxResult<Self> {
        match args[6] {
            0 => Ok(BaseFunction::GetSepcificationVersion),
            1 => Ok(BaseFunction::GetImplementationID),
            2 => Ok(BaseFunction::GetImplementationVersion),
            3 => Ok(BaseFunction::ProbeSbiExtension(args[0] as u64)),
            4 => Ok(BaseFunction::GetMachineVendorID),
            5 => Ok(BaseFunction::GetMachineArchitectureID),
            6 => Ok(BaseFunction::GetMachineImplementationID),
            _ => Err(AxError::NotFound),
        }
    }
}

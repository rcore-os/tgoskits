mod base;
mod dbcn;
mod pmu;
mod rfnc;
mod srst;

use axerrno::{AxError, AxResult};
pub use base::BaseFunction;
use dbcn::DebugConsoleFunction;
pub use pmu::PmuFunction;
pub use rfnc::RemoteFenceFunction;
use sbi_spec;
pub use srst::ResetFunction;

pub const SBI_SUCCESS: usize = 0;
pub const SBI_ERR_FAILUER: isize = -1;
pub const SBI_ERR_NOT_SUPPORTED: isize = -2;
pub const SBI_ERR_INAVLID_PARAM: isize = -3;
pub const SBI_ERR_DENIED: isize = -4;
pub const SBI_ERR_INVALID_ADDRESS: isize = -5;
pub const SBI_ERR_ALREADY_AVAILABLE: isize = -6;

/// The values returned from an SBI function call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SbiReturn {
    /// The error code(0 for success)
    pub error_code: i64,
    /// The return value if the operation is successful
    pub return_value: i64,
}

/// SBI return value conventions
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SbiReturnTyoe {
    /// Legacy(v0.1) extensions return a single value in A0, usually with the convention that 0
    /// is success and < 0 is an implementation defined error code.
    Legacy(u64),
    /// Modern extensions use the standard error code values enumerated above.
    Standard(SbiReturn),
}

/// SBI Message used to invoke the specfified SBI extension in the firmware.
#[derive(Clone, Copy, Debug)]
pub enum SbiMessage {
    /// The base SBI extension functions.
    Base(BaseFunction),
    /// The legacy GetChar extension.
    GetChar,
    /// The legacy PutChar extension.
    PutChar(usize),
    /// The SetTimer Extension
    SetTimer(usize),
    /// Handles output to the console for debug
    DebugConsole(DebugConsoleFunction),
    /// Handles system reset
    Reset(ResetFunction),
    /// The RemoteFence Extension.
    RemoteFence(RemoteFenceFunction),
    /// The PMU Extension
    PMU(PmuFunction),
}

impl SbiMessage {
    /// Creates an SbiMessage struct from the given GPRs. Intended for use from the ECALL handler
    /// and passed the saved register state from the calling OS. A7 must contain a valid SBI
    /// extension and the other A* registers will be interpreted based on the extension A7 selects.
    pub fn from_regs(args: &[usize]) -> AxResult<Self> {
        match args[7] {
            sbi_spec::base::EID_BASE => BaseFunction::from_regs(args).map(SbiMessage::Base),
            sbi_spec::legacy::LEGACY_CONSOLE_PUTCHAR => Ok(SbiMessage::PutChar(args[0])),
            sbi_spec::legacy::LEGACY_CONSOLE_GETCHAR => Ok(SbiMessage::GetChar),
            sbi_spec::legacy::LEGACY_SET_TIMER => Ok(SbiMessage::SetTimer(args[0])),
            sbi_spec::legacy::LEGACY_SHUTDOWN => Ok(SbiMessage::Reset(ResetFunction::shutdown())),
            sbi_spec::time::EID_TIME => Ok(SbiMessage::SetTimer(args[0])),
            sbi_spec::srst::EID_SRST => ResetFunction::from_regs(args).map(SbiMessage::Reset),
            sbi_spec::rfnc::EID_RFNC => {
                RemoteFenceFunction::from_args(args).map(SbiMessage::RemoteFence)
            }
            sbi_spec::pmu::EID_PMU => PmuFunction::from_regs(args).map(SbiMessage::PMU),
            _ => {
                error!("args: {:?}", args);
                error!("args[7]: {:#x}", args[7]);
                error!("EID_RFENCE: {:#x}", sbi_spec::rfnc::EID_RFNC);
                Err(AxError::NotFound)
            }
        }
    }
}

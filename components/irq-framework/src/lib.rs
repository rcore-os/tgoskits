#![no_std]

extern crate alloc;

mod action;
mod descriptor;
mod lock;
mod registry;
mod types;

pub use registry::Registry;
pub use types::{
    AutoEnable, BoxedIrqHandler, CpuId, CpuMask, CpuMaskIter, IrqAffinity, IrqContext, IrqError,
    IrqExecution, IrqHandle, IrqNumber, IrqOps, IrqOutcome, IrqRequest, IrqReturn, IrqScope,
    IrqStatus, RawIrqHandler, ShareMode,
};

#![no_std]

extern crate alloc;

mod action;
mod descriptor;
mod lock;
mod registry;
mod types;

pub use registry::Registry;
pub use types::{
    AutoEnable, CpuId, CpuMask, CpuMaskIter, IrqContext, IrqError, IrqHandle, IrqNumber, IrqOps,
    IrqOutcome, IrqRequest, IrqReturn, IrqScope, IrqStatus, RawIrqHandler, ShareMode, TriggerMode,
};

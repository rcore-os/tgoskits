#![no_std]

extern crate alloc;

mod action;
mod descriptor;
mod lock;
mod registry;
mod types;

pub use registry::Registry;
pub use types::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, AutoEnable, BoxedIrqHandler,
    CpuId, CpuMask, CpuMaskIter, HwIrq, IrqAffinity, IrqContext, IrqDomainId, IrqError,
    IrqExecution, IrqHandle, IrqId, IrqOps, IrqOutcome, IrqRequest, IrqReturn, IrqScope, IrqSource,
    IrqStatus, RawIrqHandler, ShareMode, TrapVector,
};

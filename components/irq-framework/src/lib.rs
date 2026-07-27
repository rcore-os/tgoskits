#![no_std]

extern crate alloc;

mod action;
mod descriptor;
mod lock;
mod registry;
mod types;

#[cfg(all(axtest, feature = "axtest"))]
/// Coverage tests for IRQ registration and dispatch rules.
pub mod axtest;

pub use registry::Registry;
pub use types::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, AutoEnable, BoxedIrqHandler,
    ConcurrentBoxedIrqHandler, CpuId, CpuMask, CpuMaskIter, HwIrq, IrqAffinity, IrqContext,
    IrqDomainId, IrqError, IrqExecution, IrqHandle, IrqId, IrqOps, IrqOutcome, IrqRequest,
    IrqReturn, IrqScope, IrqSource, IrqStatus, ShareMode, TrapVector,
};

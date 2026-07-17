#![no_std]

extern crate alloc;

mod action;
mod descriptor;
mod detached;
mod lock;
mod registry;
mod types;

pub use detached::{DetachedIrqAction, ReattachIrqActionError};
pub use registry::Registry;
pub use types::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, AutoEnable, BoxedIrqHandler,
    ConcurrentBoxedIrqHandler, CpuId, CpuIpiTarget, CpuMask, CpuMaskIter, HwIrq, IpiSendStatus,
    IrqAffinity, IrqContext, IrqContinuationSlot, IrqContinuationToken, IrqContinuationWake,
    IrqDomainId, IrqDrainToken, IrqDrainWake, IrqError, IrqExecution, IrqHandle, IrqId, IrqOps,
    IrqOutcome, IrqRequest, IrqReturn, IrqScope, IrqSource, IrqStatus, ShareMode, TrapVector,
};

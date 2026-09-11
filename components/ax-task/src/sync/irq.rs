//! Single-owner notification capabilities for hard IRQ producers.

pub use crate::{
    runtime::service::reclaim::quiesce_irq_wait,
    sync::irq::{
        cell::{
            IrqNotifyResult, IrqRegisterResult, IrqWaitCell, IrqWaitDrain, IrqWaitRegistration,
            IrqWaitToken,
        },
        worker::IrqWorkerWaiter,
    },
};

pub(crate) mod worker;

pub(crate) mod cell;

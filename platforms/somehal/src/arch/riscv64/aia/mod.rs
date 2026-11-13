//! Supervisor APLIC sources backed by an exclusively owned IMSIC ID namespace.
mod aplic;
mod imsic;

pub(super) use aplic::{
    activate_guest_source, claim_external_irq, complete, defer, finish_deferred, irq_set_affinity,
    retire_guest_source,
};
pub(super) use imsic::init_current_imsic;

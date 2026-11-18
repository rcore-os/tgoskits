//! Supervisor APLIC sources backed by an exclusively owned IMSIC ID namespace.
mod aplic;
mod imsic;

pub(super) use aplic::{
    activate_guest_sources, claim_external_irq, claim_host_line, complete, defer, finish_deferred,
    irq_set_affinity, release_host_line, retire_guest_source,
};
pub(super) use imsic::init_current_imsic;

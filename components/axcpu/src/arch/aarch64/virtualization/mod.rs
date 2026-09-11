//! AArch64 guest machine register state.

mod context;
pub use context::{GuestContext, GuestSystemRegisters};

mod percpu;
pub use percpu::PerCpu;

mod timer;
pub use timer::VirtualTimerState;
mod exit;
pub use exit::{Exit, ExitKind, GuestAddressError};
pub(crate) mod vcpu;
pub use vcpu::{HostIrqConfig, Vcpu};

pub use super::entry::{enter_guest, guest_vector};

/// Host-owned dispatch for a current-EL IRQ taken through the guest vector.
/// The callback runs with IRQs masked and must not retain the saved snapshot.
#[trait_ffi::def_extern_trait(mod_path = "virtualization")]
pub trait GuestHostTrap {
    /// Acknowledges and routes a current-EL physical IRQ using platform ownership.
    fn current_irq(context: crate::trap::InterruptedContext);
}

pub(crate) fn dispatch_current_irq(context: crate::trap::InterruptedContext) {
    guest_host_trap::current_irq(context);
}

mod tlb;
pub use tlb::{
    invalidate_current_guest_translations, invalidate_guest_translations_inner_shareable,
};

mod paging;
pub use paging::Stage2Config;

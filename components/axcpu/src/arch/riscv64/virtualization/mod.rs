//! RISC-V hypervisor machine state and entry primitives.

pub(super) mod context;

pub use context::{
    Exit, GuestCpuState, GuestVirtualHsCsrs, GuestVsCsrs, HypervisorCpuState, Vcpu,
    guest_page_fault_addr,
};

pub use super::entry::enter_guest;

mod percpu;
pub use percpu::PerCpu;

use crate::virtualization::VirtualizationError;

mod binding;
pub use binding::GuestBinding;

mod memory;
pub use memory::{
    GuestAccessFault, GuestPrivilege, copy_from_guest_physical, copy_from_guest_virtual,
    copy_to_guest_physical, copy_to_guest_virtual, fetch_guest_instruction,
};

mod registers;
pub use registers::GuestInterrupt;

mod exit;
pub use exit::{
    CoreInterruptNumber, Exception, ExceptionNumber, Interrupt, InterruptNumber, RegisterError,
    RegisterResult, Trap,
};

mod paging;
pub use paging::{GStageMode, invalidate_gstage_translations};

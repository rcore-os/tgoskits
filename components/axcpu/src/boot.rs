//! CPU initialization for the selected target.

pub use crate::arch::current::boot::*;
#[cfg(target_arch = "loongarch64")]
pub use crate::arch::current::entry::tlb_refill_entry;
#[cfg(target_arch = "aarch64")]
pub use crate::arch::current::mmu::{El1, El2};
#[cfg(all(target_arch = "x86_64", feature = "virtualization"))]
pub use crate::arch::current::virtualization::percpu::authorize_vmx;

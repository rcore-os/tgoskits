//! CPU publication barriers.

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "x86_64",
    target_arch = "loongarch64"
))]
pub use crate::arch::current::barrier::data_fence;
pub use crate::arch::current::barrier::synchronize_page_table_writes;
#[cfg(target_arch = "aarch64")]
pub use crate::arch::current::barrier::{data_sync_system, instruction_sync};

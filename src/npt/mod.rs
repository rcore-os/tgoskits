cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        /// The architecture-specific nested page table for two-stage address translation.
        pub type NestedPageTable<H> = arch::ExtendedPageTable<H>;
    } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
        /// The architecture-specific page table.
        pub type NestedPageTable<H> = arch::NestedPageTable<H>;
    } else if #[cfg(target_arch = "aarch64")]{
        /// The architecture-specific nested page table for two-stage address translation.
        pub type NestedPageTable<H> = arch::NestedPageTable<H>;
    }
}

mod arch;

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        /// The architecture-specific nested page table for two-stage address translation.
        pub type AxNestedPageTable<I> = crate::arch::X64NestedPageTable<I>;
    } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
        /// The architecture-specific page table.
        pub type AxNestedPageTable<I> = page_table::riscv::Sv39PageTable<I>;
    } else if #[cfg(target_arch = "aarch64")]{
        /// The architecture-specific page table.
        pub type AxNestedPageTable<I> = crate::arch::A64PageTable<I>;
    }
}

#[trait_ffi::def_extern_trait(mod_path = "hal::al")]
pub trait Memory {
    /// Convert virtual address to physical address
    /// # Safety
    /// The caller must ensure that the provided virtual address is valid and mapped.
    unsafe fn virt_to_phys(virt: *mut u8) -> usize;
    fn phys_to_virt(phys: usize) -> *mut u8;
}

#[trait_ffi::def_extern_trait(not_def_impl, mod_path = "hal::al")]
pub trait Cpu {
    fn current_cpu_id() -> usize;
    fn irq_is_enabled() -> bool;
    fn irq_set_enabled(enabled: bool);
}

#[trait_ffi::def_extern_trait(mod_path = "hal::al", not_def_impl)]
pub trait Console {
    fn early_write(bytes: &[u8]) -> usize;
    fn early_read() -> Option<u8>;
}

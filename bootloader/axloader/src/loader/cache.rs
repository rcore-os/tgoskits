/// Makes bytes written by the loader visible to the target instruction stream.
#[cfg(target_arch = "loongarch64")]
pub fn flush_instruction_cache() {
    // SAFETY: The barriers have no memory operands and do not touch the stack.
    // They order the completed image writes before the following EFI entry call.
    unsafe {
        core::arch::asm!("dbar 0", "ibar 0", options(nostack));
    }
}

/// x86 uses coherent instruction and data caches for normal memory.
#[cfg(target_arch = "x86_64")]
pub fn flush_instruction_cache() {}

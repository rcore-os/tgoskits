use core::arch::asm;

fn raise_break_exception() {
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!("int3");
        #[cfg(target_arch = "aarch64")]
        asm!("brk #0");
        #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
        asm!("ebreak");
        #[cfg(target_arch = "loongarch64")]
        asm!("break 0");
    }
}

pub fn run() -> crate::TestResult {
    raise_break_exception();
    Ok(())
}

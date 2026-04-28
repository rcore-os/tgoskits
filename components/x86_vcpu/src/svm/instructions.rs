use core::arch::asm;

pub type Result<T = ()> = core::result::Result<T, ()>;

/// Enter the guest by executing `VMRUN`.
#[inline(always)]
pub unsafe fn vmrun(vmcb_pa: u64) -> ! {
    unsafe {
        asm!("vmrun {0}", in(reg) vmcb_pa, options(noreturn, nostack));
    }
}

/// Save guest state selected by `VMSAVE` into the VMCB.
#[inline(always)]
pub unsafe fn vmsave(vmcb_pa: u64) -> Result {
    unsafe {
        asm!("vmsave {0}", in(reg) vmcb_pa, options(nostack, preserves_flags));
    }
    Ok(())
}

/// Load guest state selected by `VMLOAD` from the VMCB.
#[inline(always)]
pub unsafe fn vmload(vmcb_pa: u64) -> Result {
    unsafe {
        asm!("vmload {0}", in(reg) vmcb_pa, options(nostack, preserves_flags));
    }
    Ok(())
}

#[inline(always)]
pub unsafe fn stgi() {
    unsafe {
        asm!("stgi", options(nostack, preserves_flags));
    }
}

#[inline(always)]
pub unsafe fn clgi() {
    unsafe {
        asm!("clgi", options(nostack, preserves_flags));
    }
}

#[inline(always)]
pub unsafe fn invlpga(addr: u64, asid: u32) {
    unsafe {
        asm!(
            "invlpga {0}, {1:e}",
            in(reg) addr,
            in(reg) asid,
            options(nostack, preserves_flags),
        );
    }
}

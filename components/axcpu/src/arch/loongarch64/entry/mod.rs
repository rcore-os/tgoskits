//! Exception entry points that do not require a runtime context.

core::arch::global_asm!(include_str!("walker.S"), include_str!("tlb_refill.S"));

#[cfg(feature = "virtualization")]
pub(crate) mod guest;

unsafe extern "C" {
    fn __ax_cpu_tlb_refill();
}

/// Returns the running address of the four-level, 4 KiB TLB refill entry.
///
/// The boot owner converts this address to the physical address required by
/// TLBRENTRY and configures PWCL/PWCH before enabling page translation. The
/// handler uses only TLBRSAVE and t0; it requires neither a stack nor TLS.
/// A missing intermediate directory fills invalid EntryLo values so the
/// original load, store or fetch fault reaches the ordinary trap handler.
#[inline]
pub fn tlb_refill_entry() -> usize {
    let address;
    // SAFETY: PC-relative address materialization neither accesses memory nor
    // changes machine state. It also works before image relocations are applied.
    unsafe {
        core::arch::asm!(
            "la.pcrel {address}, {entry}",
            address = out(reg) address,
            entry = sym __ax_cpu_tlb_refill,
            options(nomem, nostack),
        );
    }
    address
}

pub(crate) mod boot;

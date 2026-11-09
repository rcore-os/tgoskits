//! Exercise the boot alignment policy with a hostile incoming SCTLR value.

use std::os::arceos::sync::IrqSaveGuard;

pub(super) fn run() {
    #[repr(align(16))]
    struct Bytes([u8; 16]);
    let bytes = Bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
    let _irq = IrqSaveGuard::new();
    let saved: u64;
    let incoming: u64;
    let observed: u64;
    let mut loaded = 0u64;
    // SAFETY: IRQ exclusion pins this EL1 CPU. Existing roots, MAIR and TCR
    // remain installed and retain code, stack and data. Only alignment checking
    // is changed before calling the real boot entry; strict-alignment kernel
    // code needs no unaligned access in this interval. Restore SCTLR before
    // asserting, so even the deliberately broken implementation is contained.
    unsafe {
        core::arch::asm!("mrs {}, sctlr_el1", out(reg) saved, options(nomem, nostack));
        core::arch::asm!("msr sctlr_el1, {0}", "isb", in(reg) saved | 2, options(nostack));
        core::arch::asm!("mrs {}, sctlr_el1", out(reg) incoming, options(nomem, nostack));
        ax_cpu::boot::El1::enable_mmu_and_caches();
        core::arch::asm!("mrs {}, sctlr_el1", out(reg) observed, options(nomem, nostack));
        if observed & 2 == 0 {
            // The eight-byte LDR stays inside this live Normal-memory array;
            // no Rust reference is created at its unaligned address.
            core::arch::asm!("ldr {0}, [{1}]", out(reg) loaded,
                in(reg) bytes.0.as_ptr().add(1), options(nostack, readonly));
        }
        core::arch::asm!("msr sctlr_el1, {0}", "isb", in(reg) saved, options(nostack));
    }
    assert_ne!(
        incoming & 2,
        0,
        "alignment fault injection did not take effect"
    );
    assert_eq!(
        observed & 2,
        0,
        "EL1 boot retained firmware alignment traps"
    );
    assert_ne!(
        observed & (1 << 23),
        0,
        "EL1 boot must preserve SPAN policy"
    );
    assert_eq!(loaded, u64::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8]));
    drop(_irq);
    std::println!("CPU_ALIGNMENT_POLICY_OK");
}

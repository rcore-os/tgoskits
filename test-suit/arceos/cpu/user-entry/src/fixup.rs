//! Linker-owned relative fixups must remain valid without runtime sorting.

core::arch::global_asm!(
    ".pushsection .text.cpu_fixup_test, \"ax\"",
    ".balign 64",
    ".global cpu_fixup_first",
    "cpu_fixup_first:",
    "mov x0, xzr",
    "12: ldr x1, [x0]",
    "13: mov x0, #1",
    "ret",
    ".balign 64",
    ".global cpu_fixup_second",
    "cpu_fixup_second:",
    "mov x0, xzr",
    "22: ldr x1, [x0]",
    "23: mov x0, #2",
    "ret",
    ".popsection",
    // Deliberately reverse address order. Each relocation is relative to its
    // own field, so sorting raw pairs changes the resolved instruction address.
    ".pushsection __nofault_ex_table, \"a\"",
    ".balign 4",
    ".long 22b - .",
    ".long 23b - .",
    ".long 12b - .",
    ".long 13b - .",
    ".popsection",
);
unsafe extern "C" {
    fn cpu_fixup_first() -> usize;
    fn cpu_fixup_second() -> usize;
}

pub fn run() {
    use std::os::arceos::{modules::ax_hal, sync::IrqSaveGuard};
    let table = ax_hal::paging::PageTable::new(ax_hal::paging::PagingAllocator).unwrap();
    let _irq = IrqSaveGuard::new();
    struct RestoreRoot(u64);
    impl Drop for RestoreRoot {
        fn drop(&mut self) {
            // SAFETY: IRQ exclusion retains this CPU and the original complete
            // TTBR0 image. The temporary table is still alive through this drop.
            unsafe {
                core::arch::asm!("msr ttbr0_el1, {}", in(reg) self.0, options(nostack));
            }
            ax_cpu::mmu::flush_tlb(None);
        }
    }
    let saved;
    // SAFETY: the test owns this IRQ-disabled EL1 window. Retain the entire
    // TTBR0 value, including ASID, rather than assuming boot left it zero.
    unsafe {
        core::arch::asm!("mrs {}, ttbr0_el1", out(reg) saved, options(nomem, nostack));
    }
    let _restore = RestoreRoot(saved);
    // SAFETY: this owned empty root is valid RAM and outlives all probes. A null
    // access must encounter an invalid descriptor, not walk physical address 0
    // and cause a platform-dependent synchronous external abort.
    unsafe {
        ax_cpu::mmu::write_user_page_table(table.root_paddr());
    }
    ax_cpu::mmu::flush_tlb(None);
    // SAFETY: both helpers fault on a genuinely unmapped null page and declare
    // linker-owned recovery entries. Real CPU vectors and the kernel stack
    // anchor are installed, and the restoration guard precedes table release.
    unsafe {
        assert_eq!(cpu_fixup_first(), 1);
        assert_eq!(cpu_fixup_second(), 2);
    }
}

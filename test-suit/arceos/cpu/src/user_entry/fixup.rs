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
    // The null page stays unmapped for this window: a null access must meet an
    // invalid descriptor, not walk physical address 0 and cause a
    // platform-dependent synchronous external abort.
    let _table = super::empty_user_table::EmptyUserTable::install();
    // SAFETY: both helpers fault on a genuinely unmapped null page and declare
    // linker-owned recovery entries. Real CPU vectors and the kernel stack
    // anchor are installed, and the restoration guard precedes table release.
    unsafe {
        assert_eq!(cpu_fixup_first(), 1);
        assert_eq!(cpu_fixup_second(), 2);
    }
}

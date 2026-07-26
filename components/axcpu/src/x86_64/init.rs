//! Helper functions to initialize the CPU states on systems bootstrapping.

/// Initializes trap handling on the current CPU.
///
/// In detail, it initializes the GDT, IDT on x86_64 platforms. If the `uspace`
/// feature is enabled, it also initializes relevant model-specific registers to
/// configure the handler for `syscall` instruction.
///
/// # Notes
/// Before calling this function, the platform entry path must have installed
/// and verified the current CPU area. Architecture trap initialization is not
/// a second per-CPU binder.
pub fn init_trap() {
    #[cfg(feature = "exception-table")]
    crate::exception_table::init_exception_table();
    super::gdt::init();
    super::idt::init();
    #[cfg(feature = "uspace")]
    super::uspace::init_syscall();
}

//! Helper functions to initialize the CPU states on systems bootstrapping.

pub use super::gdt::init_gdt;
pub use super::idt::init_idt;

#[cfg(feature = "uspace")]
pub use super::syscall::init_syscall;

/// Initializes trap handling on the current CPU.
///
/// `cpu_id` indicates the CPU ID of the current CPU.
///
/// In detail, it initializes the GDT, IDT on x86_64 platforms ([`init_gdt`] and
/// [`init_idt`]). If the `uspace` feature is enabled, it also initializes
/// relevant model-specific registers to configure the handler for `syscall`
/// instruction ([`init_syscall`]).
///
/// It also calls the initialization function of the [`percpu`] crate to use the
/// per-CPU data.
pub fn init_trap(cpu_id: usize) {
    // it's safe to call this function multiple times, the `percpu` crate
    // guarantees the actual initialization is only done once.
    percpu::init();
    percpu::init_percpu_reg(cpu_id);
    init_gdt();
    init_idt();
    #[cfg(feature = "uspace")]
    init_syscall();
}

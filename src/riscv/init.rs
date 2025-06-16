//! Helper functions to initialize the CPU states on systems bootstrapping.

/// Initializes trap handling on the current CPU.
///
/// In detail, it initializes the trap vector on RISC-V platforms.
pub fn init_trap() {
    unsafe extern "C" {
        fn trap_vector_base();
    }
    unsafe {
        crate::asm::write_trap_vector_base(trap_vector_base as usize);
    }
}

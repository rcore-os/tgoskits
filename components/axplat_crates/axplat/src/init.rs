//! Platform initialization.

/// Platform initialization interface.
#[def_plat_interface]
pub trait InitIf {
    /// Initializes the platform at the early stage for the primary core.
    ///
    /// This function should be called immediately after the kernel has booted,
    /// and performed earliest platform configuration and initialization (e.g.,
    /// early console, clocking).
    ///
    /// # Arguments
    ///
    /// * `cpu_id` is the logical CPU ID (0, 1, ..., N-1, N is the number of CPU
    /// cores on the platform).
    /// * `arg` is passed from the bootloader (typically the device tree blob
    /// address).
    ///
    /// # Before calling this function
    ///
    /// * CPU is booted in the kernel mode.
    /// * Early page table is set up, virtual memory is enabled.
    /// * CPU-local data is initialized.
    ///
    /// # After calling this function
    ///
    /// * Exception & interrupt handlers are set up.
    /// * Early console is initialized.
    /// * Current monotonic time and wall time can be obtained.
    fn init_early(cpu_id: usize, arg: usize);

    /// Initializes the platform at the early stage for secondary cores.
    ///
    /// See [`init_early`] for details.
    #[cfg(feature = "smp")]
    fn init_early_secondary(cpu_id: usize);

    /// Initializes the platform at the later stage for the primary core.
    ///
    /// This function should be called after the kernel has done part of its
    /// initialization (e.g, logging, memory management), and finalized the rest of
    /// platform configuration and initialization.
    ///
    /// # Arguments
    ///
    /// * `cpu_id` is the logical CPU ID (0, 1, ..., N-1, N is the number of CPU
    /// cores on the platform).
    /// * `arg` is passed from the bootloader (typically the device tree blob
    /// address).
    ///
    /// # Before calling this function
    ///
    /// * Kernel logging is initialized.
    /// * Fine-grained kernel page table is set up (if applicable).
    /// * Physical memory allocation is initialized (if applicable).
    ///
    /// # After calling this function
    ///
    /// * Interrupt controller is initialized (if applicable).
    /// * Timer interrupts are enabled (if applicable).
    /// * Other essential peripherals are initialized.
    fn init_later(cpu_id: usize, arg: usize);

    /// Initializes the platform at the later stage for secondary cores.
    ///
    /// See [`init_later`] for details.
    #[cfg(feature = "smp")]
    fn init_later_secondary(cpu_id: usize);
}

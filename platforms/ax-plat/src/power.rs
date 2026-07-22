//! Power management.

/// Power management interface.
#[def_plat_interface]
pub trait PowerIf {
    /// Bootstraps the given CPU core with the given initial stack (in physical
    /// address).
    ///
    /// Where `cpu_id` is the logical CPU ID (0, 1, ..., N-1, N is the number of
    /// CPU cores on the platform).
    #[cfg(feature = "smp")]
    fn cpu_boot(cpu_id: usize, stack_top_paddr: usize);

    /// Shutdown the whole system.
    fn system_off() -> !;

    /// Reset the whole system.
    fn system_reset() -> !;

    /// Get the number of CPU cores available on this platform.
    ///
    /// The platform should either get this value statically from its
    /// configuration or dynamically by platform-specific methods.
    ///
    /// For statically configured platforms, by convention, this value should be
    /// the same as `MAX_CPU_NUM` defined in the platform configuration.
    fn cpu_num() -> usize;

    /// Returns the firmware or architecture hardware ID for one logical CPU.
    ///
    /// The logical ID is the dense host scheduler index. The returned value is
    /// an MPIDR on AArch64, a hart ID on RISC-V, or the corresponding firmware
    /// CPU identifier on other architectures.
    fn cpu_hardware_id(cpu_id: usize) -> Option<usize>;
}

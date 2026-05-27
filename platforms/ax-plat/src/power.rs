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

    /// Get the number of CPU cores available on this platform.
    ///
    /// The platform should either get this value statically from its
    /// configuration or dynamically by platform-specific methods.
    ///
    /// For statically configured platforms, by convention, this value should be
    /// the same as `MAX_CPU_NUM` defined in the platform configuration.
    fn cpu_num() -> usize;
}

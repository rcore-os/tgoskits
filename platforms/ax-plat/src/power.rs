//! Power management.

/// Power management interface.
#[def_plat_interface]
pub trait PowerIf {
    /// Requests that the platform release the given CPU core.
    ///
    /// Where `cpu_id` is the logical CPU ID (0, 1, ..., N-1, N is the number of
    /// CPU cores on the platform). The platform boot layer owns the secondary
    /// stack and boot record; the OS runtime supplies only the logical target.
    #[cfg(feature = "smp")]
    fn cpu_boot(cpu_id: usize);

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
}

//! CPU topology.

/// CPU topology interface.
#[def_plat_interface]
pub trait CpuTopologyIf {
    /// Maps a firmware or hardware CPU ID to the dense logical index used by
    /// the runtime.
    ///
    /// The mapping must use the same CPU order as per-CPU runtime state.
    /// Hardware IDs are architecture-specific values such as RISC-V hart IDs,
    /// AArch64 MPIDRs, or x86 APIC IDs.
    fn resolve_cpu_index(hardware_id: usize) -> Option<usize>;

    /// Returns the boot-time compute capacity of a logical CPU in `0..=1024`.
    ///
    /// The strongest CPU has capacity 1024 when complete firmware data
    /// contains at least one nonzero capacity.
    /// Incomplete or unavailable capacity data gives 1024 for all CPUs.
    /// Integer normalization may produce zero; consumers must not use this
    /// value as a divisor without handling zero. This is an immutable boot
    /// estimate, not current frequency, thermal pressure, or available load.
    /// Returns `None` before topology publication or for an invalid index.
    fn cpu_capacity(cpu_index: usize) -> Option<u16>;
}

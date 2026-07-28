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
}

use ax_plat::cpu::CpuTopologyIf;

struct CpuTopologyImpl;

#[impl_plat_interface]
impl CpuTopologyIf for CpuTopologyImpl {
    fn resolve_cpu_index(hardware_id: usize) -> Option<usize> {
        somehal::smp::cpu_id_to_idx(hardware_id)
    }
}

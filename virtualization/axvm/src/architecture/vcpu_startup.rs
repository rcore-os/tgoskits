//! Secondary-vCPU startup configuration shared by CPU-up architectures.

use axvm_types::{GuestPhysAddr, VmArchVcpuOps, VmVcpuState};

use crate::{
    AxVmResult,
    vcpu::{AxVCpu, map_vcpu_backend_error},
};

pub(crate) fn configure_reserved_vcpu_startup<A, F>(
    vcpu: &AxVCpu<A>,
    entry: GuestPhysAddr,
    configure_args: F,
) -> AxVmResult
where
    A: VmArchVcpuOps,
    F: FnOnce(&mut A),
{
    // The reserved runtime task slot and `Starting` state exclusively own this inactive
    // backend. A CPU-up exit is still handled with the caller vCPU recorded as current, so
    // installing the target as current here would be a nested vCPU operation. Per-CPU binding
    // belongs to the target task when it first runs on its selected host CPU.
    vcpu.transition_state(VmVcpuState::Starting, VmVcpuState::Starting)?;
    let arch_vcpu = vcpu.get_arch_vcpu();
    arch_vcpu
        .set_entry(entry)
        .map_err(|error| map_vcpu_backend_error("set vCPU entry", error))?;
    configure_args(arch_vcpu);
    Ok(())
}

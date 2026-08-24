//! Secondary-vCPU startup configuration shared by CPU-up architectures.

use axvm_types::{GuestPhysAddr, VmArchVcpuOps};

use crate::{AxVmResult, vcpu::AxVCpu};

pub(crate) fn configure_reserved_vcpu_startup<A, F>(
    vcpu: &AxVCpu<A>,
    entry: GuestPhysAddr,
    configure_args: F,
) -> AxVmResult
where
    A: VmArchVcpuOps,
    F: FnOnce(&mut A),
{
    vcpu.configure_reserved_startup(entry, configure_args)
}

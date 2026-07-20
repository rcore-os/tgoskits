//! Fixed host placement for hardware-forwarded AArch64 physical IRQs.

use alloc::vec::Vec;

use axvm_types::PhysicalInterruptPolicy;

use crate::{
    AxVmError, AxVmResult,
    config::AxVMConfig,
    vm::prepare::vcpus::{VcpuPlacement, vcpu_placements_from_config},
};

pub(super) fn normalize_hardware_forwarded_vcpu_cpu_sets(config: &mut AxVMConfig) -> AxVmResult {
    if config.physical_interrupt_policy() != PhysicalInterruptPolicy::HardwareForwarded {
        return Ok(());
    }

    let placements = vcpu_placements_from_config(config);
    let cpu_sets = resolve_hardware_forwarded_vcpu_cpu_sets(
        &placements,
        crate::percpu::enabled_cpu_mask(),
        super::capabilities::logical_cpu_id,
    )?;
    config.phys_cpu_ls.set_guest_cpu_sets(cpu_sets);
    Ok(())
}

impl VcpuPlacement {
    pub(crate) fn fixed_host_cpu(self) -> AxVmResult<usize> {
        let mask = self.phys_cpu_set.ok_or_else(|| {
            AxVmError::invalid_config(alloc::format!(
                "vCPU {} has no fixed host CPU mask",
                self.id
            ))
        })?;
        if mask.count_ones() != 1 {
            return Err(AxVmError::invalid_config(alloc::format!(
                "vCPU {} requires one fixed host CPU, but mask {mask:#x} does not select exactly \
                 one CPU",
                self.id
            )));
        }
        Ok(mask.trailing_zeros() as usize)
    }
}

fn resolve_hardware_forwarded_vcpu_cpu_sets(
    placements: &[VcpuPlacement],
    available_cpu_mask: usize,
    mut logical_cpu_id: impl FnMut(usize) -> Option<usize>,
) -> AxVmResult<Vec<usize>> {
    placements
        .iter()
        .copied()
        .map(|placement| {
            let mask = if let Some(mask) = placement.phys_cpu_set {
                placement.fixed_host_cpu()?;
                mask
            } else {
                let host_cpu = logical_cpu_id(placement.phys_cpu_id).ok_or_else(|| {
                    AxVmError::invalid_config(alloc::format!(
                        "vCPU {} hardware ID {:#x} is not present in the host CPU topology",
                        placement.id,
                        placement.phys_cpu_id
                    ))
                })?;
                host_cpu_mask(placement.id, host_cpu)?
            };
            if mask & available_cpu_mask != mask {
                return Err(AxVmError::invalid_config(alloc::format!(
                    "vCPU {} requires host CPU mask {mask:#x}, but AxVM initialized only \
                     {available_cpu_mask:#x}",
                    placement.id
                )));
            }
            Ok(mask)
        })
        .collect()
}

fn host_cpu_mask(vcpu_id: usize, host_cpu: usize) -> AxVmResult<usize> {
    let shift = u32::try_from(host_cpu).map_err(|_| unrepresentable_cpu(vcpu_id, host_cpu))?;
    1usize
        .checked_shl(shift)
        .ok_or_else(|| unrepresentable_cpu(vcpu_id, host_cpu))
}

fn unrepresentable_cpu(vcpu_id: usize, host_cpu: usize) -> AxVmError {
    AxVmError::invalid_config(alloc::format!(
        "vCPU {vcpu_id} resolves to host CPU {host_cpu}, which cannot be represented in a CPU mask"
    ))
}

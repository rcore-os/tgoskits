//! Architecture-neutral vCPU collection construction and setup.

use alloc::{boxed::Box, sync::Arc, vec::Vec};

use axdevice::{
    DeviceManagerError, DeviceManagerResult, VcpuInterruptAffinity, VcpuInterruptId,
    VcpuInterruptPort, VcpuInterruptWake,
};
use axvm_types::VmArchVcpuOps;

use super::super::{AxVCpuRef, AxVMResources, VCpu};
use crate::AxVmResult;

#[derive(Clone, Copy, Debug)]
pub(crate) struct VcpuPlacement {
    pub(crate) id: usize,
    pub(crate) phys_cpu_set: Option<usize>,
    pub(crate) phys_cpu_id: usize,
}

pub(crate) struct PreparedVcpus {
    vcpus: Vec<AxVCpuRef>,
}

impl PreparedVcpus {
    pub(crate) fn create(
        vm_id: usize,
        placements: &[VcpuPlacement],
        mut build_config: impl FnMut(
            VcpuPlacement,
        ) -> AxVmResult<
            <crate::arch::ArchVCpu as VmArchVcpuOps>::CreateConfig,
        >,
    ) -> AxVmResult<Self> {
        debug!("id: {vm_id}, vCPU placements: {placements:#x?}");

        let mut vcpus = Vec::with_capacity(placements.len());
        for placement in placements.iter().copied() {
            trace!(
                "Creating VM[{vm_id}] vCPU[{}] for physical CPU {}",
                placement.id, placement.phys_cpu_id
            );
            let arch_config = build_config(placement)?;

            // FIXME: VCpu is neither `Send` nor `Sync` by design, check whether
            // 1. we should make it `Send` and `Sync`, or
            // 2. we can guarantee that no cross-thread access is performed
            #[allow(clippy::arc_with_non_send_sync)]
            vcpus.push(Arc::new(VCpu::new(
                vm_id,
                placement.id,
                placement.phys_cpu_set,
                arch_config,
            )?));
        }

        Ok(Self { vcpus })
    }

    pub(crate) fn setup(
        &self,
        resources: &AxVMResources,
        mut build_config: impl FnMut(
            &crate::config::AxVMConfig,
            &[crate::vm::VMMemoryRegion],
        ) -> AxVmResult<
            <crate::arch::ArchVCpu as VmArchVcpuOps>::SetupConfig,
        >,
    ) -> AxVmResult {
        for vcpu in &self.vcpus {
            let entry = if vcpu.id() == 0 {
                resources.config.bsp_entry()
            } else {
                resources.config.ap_entry()
            };

            debug!("Setting up vCPU[{}] entry at {:#x}", vcpu.id(), entry);
            vcpu.setup(
                entry,
                resources.nested_paging,
                build_config(&resources.config, &resources.memory_regions)?,
            )?;
        }
        Ok(())
    }

    pub(crate) fn interrupt_ports(
        &self,
        vm_id: usize,
        placements: &[VcpuPlacement],
    ) -> AxVmResult<Vec<VcpuInterruptPort>> {
        let mut ports = Vec::with_capacity(self.vcpus.len());
        for vcpu in &self.vcpus {
            let placement = placements
                .iter()
                .find(|placement| placement.id == vcpu.id())
                .ok_or_else(|| {
                    crate::ax_err_type!(
                        BadState,
                        alloc::format!("vCPU {} has no interrupt affinity", vcpu.id())
                    )
                })?;
            ports.push(VcpuInterruptPort::new(
                VcpuInterruptId::new(vcpu.id()),
                VcpuInterruptAffinity::new(placement.phys_cpu_id as u64),
                Arc::new(RuntimeVcpuWake {
                    vm_id,
                    vcpu_id: vcpu.id(),
                }),
            ));
        }
        Ok(ports)
    }

    pub(crate) fn into_boxed_slice(self) -> Box<[AxVCpuRef]> {
        self.vcpus.into_boxed_slice()
    }
}

struct RuntimeVcpuWake {
    vm_id: usize,
    vcpu_id: usize,
}

impl VcpuInterruptWake for RuntimeVcpuWake {
    fn wake(&self) -> DeviceManagerResult {
        crate::runtime::vcpus::wake_vcpu(self.vm_id, self.vcpu_id).map_err(|error| {
            DeviceManagerError::UnexpectedResponse {
                operation: "wake vCPU interrupt port",
                detail: alloc::format!("{error}"),
            }
        })
    }
}

pub(crate) fn vcpu_placements(resources: &AxVMResources) -> Vec<VcpuPlacement> {
    resources
        .config
        .phys_cpu_ls
        .get_vcpu_affinities_pcpu_ids()
        .into_iter()
        .map(|(id, phys_cpu_set, phys_cpu_id)| VcpuPlacement {
            id,
            phys_cpu_set,
            phys_cpu_id,
        })
        .collect()
}

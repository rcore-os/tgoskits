//! Architecture-neutral vCPU collection construction and setup.

use std::{boxed::Box, vec::Vec};

use axvm_types::VmArchVcpuOps;

use super::super::{AxVMResources, VCpu};
use crate::AxVmResult;

#[derive(Clone, Copy, Debug)]
pub(crate) struct VcpuPlacement {
    pub(crate) id: usize,
    pub(crate) phys_cpu_set: Option<usize>,
    pub(crate) phys_cpu_id: usize,
}

pub(crate) struct PreparedVcpus {
    vcpus: Vec<VCpu>,
}

impl PreparedVcpus {
    pub(crate) fn create(
        vm_id: usize,
        placements: &[VcpuPlacement],
        mut build_config: impl FnMut(
            VcpuPlacement,
        ) -> AxVmResult<
            <crate::arch::current::ArchVCpu as VmArchVcpuOps>::CreateConfig,
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

            vcpus.push(VCpu::new(
                vm_id,
                placement.id,
                placement.phys_cpu_set,
                arch_config,
            )?);
        }

        Ok(Self { vcpus })
    }

    pub(crate) fn setup(
        &mut self,
        resources: &AxVMResources,
        config: &crate::config::AxVMConfig,
        mut build_config: impl FnMut(
            &crate::config::AxVMConfig,
            &[crate::vm::VMMemoryRegion],
        ) -> AxVmResult<
            <crate::arch::current::ArchVCpu as VmArchVcpuOps>::SetupConfig,
        >,
    ) -> AxVmResult {
        for vcpu in &mut self.vcpus {
            let entry = if vcpu.id() == 0 {
                config.bsp_entry()
            } else {
                config.ap_entry()
            };

            debug!("Setting up vCPU[{}] entry at {:#x}", vcpu.id(), entry);
            vcpu.setup(
                entry,
                resources.nested_paging,
                build_config(config, &resources.memory_regions)?,
            )?;
        }
        Ok(())
    }

    pub(crate) fn into_boxed_slice(self) -> Box<[VCpu]> {
        self.vcpus.into_boxed_slice()
    }
}

impl<'a> IntoIterator for &'a mut PreparedVcpus {
    type Item = &'a mut VCpu;
    type IntoIter = std::slice::IterMut<'a, VCpu>;

    fn into_iter(self) -> Self::IntoIter {
        self.vcpus.iter_mut()
    }
}

impl AxVMResources {
    pub(crate) fn vcpu_placements(&self, config: &crate::config::AxVMConfig) -> Vec<VcpuPlacement> {
        config
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
}

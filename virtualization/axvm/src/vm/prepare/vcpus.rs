//! vCPU construction and setup for VM preparation.

use alloc::{boxed::Box, sync::Arc, vec::Vec};

use ax_errno::AxResult;
use axvm_types::{EmulatedDeviceType, GuestPhysAddr};

use super::super::{AxVCpuRef, AxVMResources, VCpu};
use crate::{
    arch::{ArchOps, CurrentArch, VcpuCreateContext, VcpuSetupContext},
    config::{GuestBootPolicy, VMBootProtocol},
};

pub(super) struct PreparedVcpus {
    vcpus: Vec<AxVCpuRef>,
}

impl PreparedVcpus {
    pub(super) fn create(
        vm_id: usize,
        resources: &AxVMResources,
        dtb_addr: Option<GuestPhysAddr>,
    ) -> AxResult<Self> {
        let vcpu_id_pcpu_sets = resources.config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let create_state = CurrentArch::new_vcpu_create_state(&vcpu_id_pcpu_sets)?;
        let firmware_boot = guest_uses_firmware_boot(resources);

        debug!("dtb_load_gpa: {dtb_addr:?}");
        debug!("id: {vm_id}, VCpuIdPCpuSets: {vcpu_id_pcpu_sets:#x?}");

        let mut vcpus = Vec::with_capacity(vcpu_id_pcpu_sets.len());
        for (vcpu_id, phys_cpu_set, phys_cpu_id) in vcpu_id_pcpu_sets {
            let arch_config = CurrentArch::build_vcpu_create_config(
                &create_state,
                VcpuCreateContext {
                    vcpu_id,
                    phys_cpu_id,
                    dtb_addr,
                    firmware_boot,
                },
            )?;

            // FIXME: VCpu is neither `Send` nor `Sync` by design, check whether
            // 1. we should make it `Send` and `Sync`, or
            // 2. we can guarantee that no cross-thread access is performed
            #[allow(clippy::arc_with_non_send_sync)]
            vcpus.push(Arc::new(VCpu::new(
                vm_id,
                vcpu_id,
                phys_cpu_set,
                arch_config,
            )?));
        }

        Ok(Self { vcpus })
    }

    pub(super) fn setup(&self, resources: &AxVMResources) -> AxResult {
        for vcpu in &self.vcpus {
            let setup_config = CurrentArch::build_vcpu_setup_config(VcpuSetupContext {
                interrupt_mode: resources.config.interrupt_mode(),
                emulates_console: resources
                    .config
                    .emu_devices()
                    .iter()
                    .any(|dev| dev.emu_type == EmulatedDeviceType::Console),
                passthrough_ports: resources.config.pass_through_ports(),
                memory_regions: &resources.memory_regions,
                firmware_boot: guest_uses_firmware_boot(resources),
            })?;

            let entry = if vcpu.id() == 0 {
                resources.config.bsp_entry()
            } else {
                resources.config.ap_entry()
            };

            debug!("Setting up vCPU[{}] entry at {:#x}", vcpu.id(), entry);

            vcpu.setup(entry, resources.nested_paging, setup_config)?;
        }
        Ok(())
    }

    pub(super) fn into_boxed_slice(self) -> Box<[AxVCpuRef]> {
        self.vcpus.into_boxed_slice()
    }
}

fn guest_uses_firmware_boot(resources: &AxVMResources) -> bool {
    matches!(
        resources.config.boot_policy(),
        GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: VMBootProtocol::Uefi,
        }
    )
}

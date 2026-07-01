//! vCPU construction and setup for VM preparation.

use alloc::{boxed::Box, sync::Arc, vec::Vec};

use ax_errno::AxResult;
#[cfg(target_arch = "x86_64")]
use axvm_types::EmulatedDeviceType;
use axvm_types::GuestPhysAddr;

use super::super::{AxVCpuRef, AxVMResources, VCpu};
#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
use crate::config::VMInterruptMode;
#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
)))]
use crate::vcpu::AxArchVCpuImpl;
#[cfg(not(target_arch = "x86_64"))]
use crate::vcpu::AxVCpuCreateConfig;

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
        #[cfg(target_arch = "loongarch64")]
        let loongarch_iocsr_state = {
            let vcpu_state_count = vcpu_id_pcpu_sets
                .iter()
                .map(|(vcpu_id, ..)| *vcpu_id)
                .max()
                .map_or(0, |vcpu_id| vcpu_id + 1);
            loongarch_vcpu::LoongArchIocsrState::new(vcpu_state_count)?
        };

        debug!("dtb_load_gpa: {dtb_addr:?}");
        debug!("id: {vm_id}, VCpuIdPCpuSets: {vcpu_id_pcpu_sets:#x?}");

        let mut vcpus = Vec::with_capacity(vcpu_id_pcpu_sets.len());
        for (vcpu_id, phys_cpu_set, _pcpu_id) in vcpu_id_pcpu_sets {
            #[cfg(target_arch = "aarch64")]
            let arch_config = AxVCpuCreateConfig {
                mpidr_el1: _pcpu_id as _,
                dtb_addr: dtb_addr.unwrap_or_default().as_usize(),
            };
            #[cfg(target_arch = "riscv64")]
            let arch_config = AxVCpuCreateConfig {
                hart_id: vcpu_id as _,
                dtb_addr: dtb_addr.unwrap_or_default().as_usize(),
            };
            #[cfg(target_arch = "loongarch64")]
            let arch_config = AxVCpuCreateConfig {
                cpu_id: vcpu_id,
                dtb_addr: dtb_addr.unwrap_or_default().as_usize(),
                boot_args: resources.config.cpu_config.boot_args,
                boot_stack_top: resources.config.cpu_config.boot_stack_top,
                firmware_boot: resources.config.cpu_config.firmware_boot,
                iocsr_state: loongarch_iocsr_state.clone(),
            };

            // FIXME: VCpu is neither `Send` nor `Sync` by design, check whether
            // 1. we should make it `Send` and `Sync`, or
            // 2. we can guarantee that no cross-thread access is performed
            #[allow(clippy::arc_with_non_send_sync)]
            vcpus.push(Arc::new(VCpu::new(
                vm_id,
                vcpu_id,
                0, // Currently not used.
                phys_cpu_set,
                #[cfg(target_arch = "aarch64")]
                arch_config,
                #[cfg(target_arch = "loongarch64")]
                arch_config,
                #[cfg(target_arch = "riscv64")]
                arch_config,
                #[cfg(target_arch = "x86_64")]
                (),
            )?));
        }

        Ok(Self { vcpus })
    }

    pub(super) fn setup(&self, resources: &AxVMResources) -> AxResult {
        for vcpu in &self.vcpus {
            #[cfg(target_arch = "aarch64")]
            let setup_config = {
                let passthrough = resources.config.interrupt_mode() == VMInterruptMode::Passthrough;
                crate::vcpu::AxVCpuSetupConfig {
                    passthrough_interrupt: passthrough,
                    passthrough_timer: passthrough,
                }
            };
            #[cfg(target_arch = "loongarch64")]
            let setup_config = {
                let passthrough = resources.config.interrupt_mode() == VMInterruptMode::Passthrough;
                crate::vcpu::AxVCpuSetupConfig {
                    passthrough_interrupt: passthrough,
                    passthrough_timer: passthrough,
                    boot_args: resources.config.cpu_config.boot_args,
                    boot_stack_top: resources.config.cpu_config.boot_stack_top,
                    firmware_boot: resources.config.cpu_config.firmware_boot,
                }
            };
            #[cfg(not(any(
                target_arch = "aarch64",
                target_arch = "loongarch64",
                target_arch = "x86_64"
            )))]
            #[allow(clippy::let_unit_value)]
            let setup_config = <AxArchVCpuImpl as axvcpu::AxArchVCpu>::SetupConfig::default();
            #[cfg(target_arch = "x86_64")]
            let setup_config = {
                let mut config = crate::vcpu::AxVCpuSetupConfig {
                    emulate_com1: resources
                        .config
                        .emu_devices()
                        .iter()
                        .any(|dev| dev.emu_type == EmulatedDeviceType::Console),
                    ..Default::default()
                };
                for port in resources.config.pass_through_ports() {
                    config.add_passthrough_port_range(port.base, port.length)?;
                }
                config
            };

            let entry = if vcpu.id() == 0 {
                resources.config.bsp_entry()
            } else {
                resources.config.ap_entry()
            };

            debug!("Setting up vCPU[{}] entry at {:#x}", vcpu.id(), entry);

            vcpu.setup(
                entry,
                resources.address_space.page_table_root(),
                setup_config,
            )?;
        }
        Ok(())
    }

    pub(super) fn into_boxed_slice(self) -> Box<[AxVCpuRef]> {
        self.vcpus.into_boxed_slice()
    }
}

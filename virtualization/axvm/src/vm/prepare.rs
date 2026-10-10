//! Architecture-neutral mechanics used by architecture-owned VM initialization.

pub(crate) mod address_space;
pub(crate) mod device_plan;
pub(crate) mod devices;
pub(crate) mod vcpus;

use std::sync::Arc;

use axdevice_base::VirtualInterruptController;

use self::{devices::PreparedDevices, vcpus::PreparedVcpus};
use super::{AxVM, AxVMResources};
use crate::{config::AxVMConfig, *};

pub(crate) struct PreparedVm {
    vcpus: PreparedVcpus,
    devices: PreparedDevices,
    interrupt_controller: Arc<dyn VirtualInterruptController>,
}

impl PreparedVm {
    pub(crate) fn new(
        vcpus: PreparedVcpus,
        devices: PreparedDevices,
        interrupt_controller: Arc<dyn VirtualInterruptController>,
    ) -> Self {
        Self {
            vcpus,
            devices,
            interrupt_controller,
        }
    }
}

impl AxVM {
    pub(crate) fn prepare(&mut self) -> AxVmResult {
        crate::arch::current::CurrentArch::init_vm(self)
    }

    pub(crate) fn prepare_resources_with(
        &mut self,
        initialize: impl FnOnce(&mut AxVMResources, &AxVMConfig) -> AxVmResult<PreparedVm>,
    ) -> AxVmResult {
        let retired = self.resources.reset_transient_resources()?;
        drop(retired);
        let prepared = initialize(&mut self.resources, &self.config)?;
        self.resources.phys_cpu_ls = self.config.phys_cpu_ls.clone();
        self.resources.vcpu_list = Some(
            prepared
                .vcpus
                .into_boxed_slice()
                .into_vec()
                .into_iter()
                .map(Some)
                .collect(),
        );
        self.resources.devices = Some(Arc::new(prepared.devices.into_inner()));
        self.resources.interrupt_controller = Some(prepared.interrupt_controller);
        Ok(())
    }
}

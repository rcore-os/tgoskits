//! Architecture-neutral mechanics used by architecture-owned VM initialization.

pub(crate) mod address_space;
pub(crate) mod device_plan;
pub(crate) mod devices;
pub(crate) mod vcpus;

use std::{format, sync::Arc};

use axdevice_base::VirtualInterruptController;

use self::{devices::PreparedDevices, vcpus::PreparedVcpus};
use super::{AxVM, AxVMResources};
use crate::*;

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
    /// Sets up the VM before booting.
    pub fn prepare(self: &Arc<Self>) -> AxVmResult {
        crate::arch::CurrentArch::init_vm(self)
    }
}

pub(crate) fn complete_vm_init(
    vm: &AxVM,
    initialize: impl FnOnce(&mut AxVMResources) -> AxVmResult<PreparedVm>,
) -> AxVmResult {
    let mut machine = vm.machine.lock();
    if !matches!(
        machine.status(),
        crate::lifecycle::VmStatus::Ready | crate::lifecycle::VmStatus::Stopped
    ) {
        return ax_err!(
            BadState,
            format!("VM[{}] cannot prepare from {:?}", vm.id(), machine.status())
        );
    }
    let resources = machine
        .resources_mut()
        .ok_or_else(|| ax_err_type!(BadState, "VM resources are not available for prepare"))?;
    resources.reset_transient_resources()?;
    let prepared = match initialize(resources) {
        Ok(prepared) => prepared,
        Err(err) => {
            if let Err(reset_err) = resources.reset_transient_resources() {
                warn!(
                    "VM[{}] failed to reset transient resources after initialization error: \
                     {reset_err:?}",
                    vm.id()
                );
            }
            return Err(err);
        }
    };
    resources.phys_cpu_ls = resources.config.phys_cpu_ls.clone();
    resources.vcpu_list = Some(prepared.vcpus.into_boxed_slice());
    resources.devices = Some(Arc::new(prepared.devices.into_inner()));
    resources.interrupt_controller = Some(prepared.interrupt_controller);

    info!("VM setup: id={}", vm.id());
    Ok(())
}

pub(crate) fn validate_guest_dtb(resources: &AxVMResources) -> AxVmResult {
    if resources.config.image_config().dtb_load_gpa.is_some()
        && resources.boot_description.device_tree().is_none()
    {
        return ax_err!(
            InvalidInput,
            "DTB load GPA is configured but no guest device tree bytes are registered"
        );
    }
    Ok(())
}

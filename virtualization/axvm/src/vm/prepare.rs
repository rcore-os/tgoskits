//! Architecture-neutral mechanics used by architecture-owned VM initialization.

pub(crate) mod address_space;
pub(crate) mod devices;
pub(crate) mod vcpus;

use alloc::{format, sync::Arc};

use axdevice::InterruptTopology;

use self::{devices::PreparedDevices, vcpus::PreparedVcpus};
use super::{AxVM, AxVMResources};
use crate::{AxVmResult, ax_err, ax_err_type};

pub(crate) struct PreparedVm {
    vcpus: PreparedVcpus,
    devices: PreparedDevices,
}

impl PreparedVm {
    pub(crate) const fn new(vcpus: PreparedVcpus, devices: PreparedDevices) -> Self {
        Self { vcpus, devices }
    }
}

impl AxVM {
    /// Sets up the VM before booting.
    pub fn prepare(&self) -> AxVmResult {
        crate::arch::CurrentArch::init_vm(self)
    }
}

pub(crate) fn complete_vm_init(
    vm: &AxVM,
    interrupt_topology: Arc<InterruptTopology>,
    initialize: impl FnOnce(&mut AxVMResources, &InterruptTopology) -> AxVmResult<PreparedVm>,
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
    resources.require_host_device_claims()?;
    let prepared = match initialize(resources, &interrupt_topology) {
        Ok(prepared) => prepared,
        Err(err) => {
            if let Err(reset_err) = interrupt_topology.reset_after_failed_preparation() {
                warn!(
                    "VM[{}] failed to roll back interrupt topology after initialization error: \
                     {reset_err:?}",
                    vm.id()
                );
            }
            if let Err(reset_err) = resources.reset_transient_resources() {
                warn!(
                    "VM[{}] failed to reset transient resources after initialization error: \
                     {reset_err:?}",
                    vm.id()
                );
            }
            resources.rollback_pending_host_device_claims();
            return Err(err);
        }
    };
    resources.commit_host_device_claims()?;
    resources.phys_cpu_ls = resources.config.phys_cpu_ls.clone();
    resources.vcpu_list = Some(prepared.vcpus.into_boxed_slice());
    resources.devices = Some(Arc::new(prepared.devices.into_inner()));
    resources.interrupt_topology = Some(interrupt_topology);

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

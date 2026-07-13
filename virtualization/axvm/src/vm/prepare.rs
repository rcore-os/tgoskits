//! Architecture-neutral mechanics used by architecture-owned VM initialization.

pub(crate) mod address_space;
pub(crate) mod devices;
pub(crate) mod vcpus;

use alloc::{format, sync::Arc};

use axdevice::{DeviceFactoryRegistry, register_builtin_factories};

use self::{devices::PreparedDevices, vcpus::PreparedVcpus};
use super::{AxVM, AxVMResources};
use crate::{AxVmError, AxVmResult, ax_err, ax_err_type, irq::InterruptFabric};

pub(crate) enum VmInitRequest<'a> {
    Default,
    Provided {
        factories: &'a DeviceFactoryRegistry,
        interrupt_fabric: InterruptFabric,
    },
}

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
        crate::arch::CurrentArch::init_vm(self, VmInitRequest::Default)
    }

    /// Sets up the VM with explicit device factories and an interrupt fabric.
    pub fn prepare_with_factories(
        &self,
        factories: &DeviceFactoryRegistry,
        interrupt_fabric: InterruptFabric,
    ) -> AxVmResult {
        crate::arch::CurrentArch::init_vm(
            self,
            VmInitRequest::Provided {
                factories,
                interrupt_fabric,
            },
        )
    }
}

pub(crate) fn default_device_factories() -> AxVmResult<DeviceFactoryRegistry> {
    let mut factories = DeviceFactoryRegistry::new();
    register_builtin_factories(&mut factories)
        .map_err(|error| AxVmError::device("register built-in device factories", error))?;
    Ok(factories)
}

pub(crate) fn complete_vm_init(
    vm: &AxVM,
    interrupt_fabric: InterruptFabric,
    initialize: impl FnOnce(&mut AxVMResources, &InterruptFabric) -> AxVmResult<PreparedVm>,
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
    interrupt_fabric.validate_mode(resources.config.interrupt_mode())?;

    let prepared = match initialize(resources, &interrupt_fabric) {
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
    resources.interrupt_fabric = Some(interrupt_fabric);

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

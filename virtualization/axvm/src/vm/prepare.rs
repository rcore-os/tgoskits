//! Architecture-neutral mechanics used by architecture-owned VM initialization.

pub(crate) mod address_space;
pub(crate) mod devices;
pub(crate) mod vcpus;

use alloc::{format, sync::Arc};

use axdevice::{DeviceFactoryRegistry, FwCfgPayloadFactory, register_builtin_factories};
use axdevice_base::VirtualInterruptController;

use self::{devices::PreparedDevices, vcpus::PreparedVcpus};
use super::{AxVM, AxVMResources};
use crate::{AxVmResult, ax_err, ax_err_type};

pub(crate) enum VmInitRequest<'a> {
    Default,
    Provided {
        factories: &'a mut DeviceFactoryRegistry,
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
    pub fn prepare(self: &Arc<Self>) -> AxVmResult {
        crate::arch::CurrentArch::init_vm(self, VmInitRequest::Default)
    }

    /// Sets up the VM with explicit device factories.
    ///
    /// The architecture still owns the machine interrupt controller and adds
    /// its controller factories to this registry.
    pub fn prepare_with_factories(
        self: &Arc<Self>,
        factories: &mut DeviceFactoryRegistry,
    ) -> AxVmResult {
        crate::machine::register_machine_device_factories(self, factories)?;
        crate::arch::CurrentArch::init_vm(self, VmInitRequest::Provided { factories })
    }
}

pub(crate) fn default_device_factories(vm: &AxVM) -> AxVmResult<DeviceFactoryRegistry> {
    let mut factories = DeviceFactoryRegistry::new();
    register_builtin_factories(&mut factories)?;
    crate::machine::register_machine_device_factories(vm, &mut factories)?;
    Ok(factories)
}

/// Adds VM-local boot-payload factories to an architecture's static registry.
///
/// Only architectures that expose such a configured device should call this:
/// keeping the common RISC-V path independent of unrelated boot-payload state
/// avoids introducing a VM-lock dependency before its interrupt fabric exists.
#[allow(dead_code)]
pub(crate) fn register_boot_payload_factories(
    vm: &AxVM,
    factories: &mut DeviceFactoryRegistry,
) -> AxVmResult {
    if let Some(payload) = vm.fw_cfg_payload() {
        factories.register(Arc::new(FwCfgPayloadFactory::new(payload)))?;
    }
    Ok(())
}

pub(crate) fn complete_vm_init(
    vm: &AxVM,
    interrupt_controller: Arc<dyn VirtualInterruptController>,
    initialize: impl FnOnce(
        &mut AxVMResources,
        &dyn VirtualInterruptController,
    ) -> AxVmResult<PreparedVm>,
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
    let prepared = match initialize(resources, interrupt_controller.as_ref()) {
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
    resources.interrupt_controller = Some(interrupt_controller);

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

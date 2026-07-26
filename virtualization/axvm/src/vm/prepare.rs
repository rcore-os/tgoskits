//! Architecture-neutral mechanics used by architecture-owned VM initialization.

pub(crate) mod address_space;
pub(crate) mod devices;
pub(crate) mod vcpus;

use alloc::{format, sync::Arc};

use axdevice::{DeviceFactoryRegistry, FwCfgPayloadFactory, register_builtin_factories};
use axvm_types::VMInterruptMode;

use self::{devices::PreparedDevices, vcpus::PreparedVcpus};
use super::{AxVM, AxVMResources};
use crate::{AxVmResult, ax_err, ax_err_type, irq::InterruptFabric};

/// Rebuilds the per-VM device factory registry and interrupt fabric for one
/// prepare generation.
///
/// The host hypervisor glue (AxVisor) implements this trait and installs it on
/// the VM via [`AxVM::install_prepare_profile`] before the first prepare. The
/// axvm core owns only the generic capability; the concrete virtio-net factory,
/// echo backend and RX worker live in the OS glue.
///
/// Storing the profile on the VM (rather than rebuilding from a global slot) is
/// what makes [`AxVM::reset`] and stopped-start re-prepare with the same glue
/// instead of falling back to the empty default factory registry: both paths go
/// through [`AxVM::prepare`], which consults the installed profile.
pub trait PrepareProfile: Send + Sync {
    /// The interrupt mode the built fabrics target.
    fn interrupt_mode(&self) -> VMInterruptMode;

    /// Builds a fresh factory registry and interrupt fabric for `generation`.
    ///
    /// `generation` is the new [`AxVM::prepare_generation`] for this prepare;
    /// the fabric's IRQ sink must be stamped with it so stale sinks can be
    /// rejected after a later re-prepare.
    fn build(&self, generation: usize) -> AxVmResult<(DeviceFactoryRegistry, InterruptFabric)>;

    /// Releases profile-owned runtime resources after the VM reaches `Stopped`.
    ///
    /// AxVM calls this after dropping its lifecycle lock, so implementations may
    /// wake and join workers that query VM state while exiting.
    fn on_stopped(&self) {}
}

pub(crate) enum VmInitRequest<'a> {
    Default,
    Provided {
        factories: &'a DeviceFactoryRegistry,
        interrupt_fabric: InterruptFabric,
    },
}

/// The architecture-owned inputs to the common device preparation path.
///
/// Every default VM creation path must produce this object before calling
/// [`complete_vm_init`].  It keeps factory registration and interrupt-fabric
/// selection together, so an architecture cannot accidentally construct
/// devices with a fabric different from the one that resolved their IRQs.
pub(crate) struct ArchDeviceBootstrap {
    factories: DeviceFactoryRegistry,
    interrupt_fabric: InterruptFabric,
}

impl ArchDeviceBootstrap {
    /// Creates one complete architecture device bootstrap result.
    pub(crate) const fn new(
        factories: DeviceFactoryRegistry,
        interrupt_fabric: InterruptFabric,
    ) -> Self {
        Self {
            factories,
            interrupt_fabric,
        }
    }

    /// Splits the bootstrap result for the common preparation path.
    pub(crate) fn into_parts(self) -> (DeviceFactoryRegistry, InterruptFabric) {
        (self.factories, self.interrupt_fabric)
    }
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
        if let Some(profile) = self.installed_profile() {
            let generation = self.next_prepare_generation();
            let (factories, fabric) = profile.build(generation)?;
            return self.prepare_with_factories(&factories, fabric);
        }
        self.next_prepare_generation();
        crate::arch::CurrentArch::init_vm(self, VmInitRequest::Default)
    }

    /// Sets up the VM with explicit device factories and an interrupt fabric.
    pub fn prepare_with_factories(
        self: &Arc<Self>,
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
    register_builtin_factories(&mut factories)?;
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

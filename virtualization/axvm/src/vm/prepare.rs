//! VM preparation orchestration.

mod address_space;
mod devices;
mod vcpus;

use alloc::{format, sync::Arc};

use ax_errno::{AxResult, ax_err, ax_err_type};
use axdevice::{DeviceFactoryRegistry, register_builtin_factories};

use self::{devices::PreparedDevices, vcpus::PreparedVcpus};
use super::AxVM;
use crate::irq::InterruptFabric;

impl AxVM {
    /// Sets up the VM before booting.
    pub fn prepare(&self) -> AxResult {
        self.ensure_resources_ready()?;
        let mut factories = DeviceFactoryRegistry::new();
        register_builtin_factories(&mut factories)?;
        let interrupt_mode = self.interrupt_mode();
        #[cfg(target_arch = "riscv64")]
        let interrupt_fabric = {
            let machine = self.machine.lock();
            let resources = machine.resources().ok_or_else(|| {
                ax_err_type!(
                    BadState,
                    "VM resources are not available for RISC-V IRQ setup"
                )
            })?;
            crate::irq::riscv::configure(
                &mut factories,
                interrupt_mode,
                resources.config.emu_devices(),
            )?
        };
        #[cfg(not(target_arch = "riscv64"))]
        let interrupt_fabric = InterruptFabric::new(interrupt_mode);

        self.prepare_with_factories(&factories, interrupt_fabric)
    }

    /// Sets up the VM with explicit device factories and an interrupt fabric.
    pub fn prepare_with_factories(
        &self,
        factories: &DeviceFactoryRegistry,
        interrupt_fabric: InterruptFabric,
    ) -> AxResult {
        self.ensure_resources_ready()?;
        let mut machine = self.machine.lock();
        if !matches!(
            machine.status(),
            crate::lifecycle::VmStatus::Ready | crate::lifecycle::VmStatus::Stopped
        ) {
            return ax_err!(
                BadState,
                format!(
                    "VM[{}] cannot prepare from {:?}",
                    self.id(),
                    machine.status()
                )
            );
        }
        let resources = machine
            .resources_mut()
            .ok_or_else(|| ax_err_type!(BadState, "VM resources are not available for prepare"))?;
        if resources.vcpu_list.is_some()
            || resources.devices.is_some()
            || resources.interrupt_fabric.is_some()
        {
            resources.reset_transient_resources()?;
        }
        interrupt_fabric.validate_mode(resources.config.interrupt_mode())?;

        let dtb_addr = resources.config.image_config().dtb_load_gpa;
        let vcpus = PreparedVcpus::create(self.id(), resources, dtb_addr)?;
        let devices = PreparedDevices::build(self, resources, factories, &interrupt_fabric)?;

        if dtb_addr.is_some() && resources.boot_description.device_tree().is_none() {
            return ax_err!(
                InvalidInput,
                "DTB load GPA is configured but no guest device tree bytes are registered"
            );
        }

        address_space::map_guest_address_space(self, resources, devices.devices())?;
        vcpus.setup(resources)?;

        resources.phys_cpu_ls = resources.config.phys_cpu_ls.clone();
        resources.vcpu_list = Some(vcpus.into_boxed_slice());
        resources.devices = Some(Arc::new(devices.into_inner()));
        resources.interrupt_fabric = Some(interrupt_fabric);

        info!("VM setup: id={}", self.id());
        Ok(())
    }
}

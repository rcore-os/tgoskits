//! Device construction for VM preparation.

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use alloc::{format, sync::Arc};

use ax_errno::AxResult;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use ax_errno::ax_err_type;
use axdevice::{AxVmDeviceConfig, AxVmDevices, DeviceBuildContext, DeviceFactoryRegistry};
#[cfg(target_arch = "aarch64")]
use axdevice_base::DeviceRegistry as _;
#[cfg(target_arch = "x86_64")]
use axdevice_base::{BaseDeviceOps, DeviceRegistry as _, PortDeviceAdapter};

use super::super::{AxVM, AxVMResources};
#[cfg(target_arch = "aarch64")]
use crate::config::VMInterruptMode;
use crate::irq::InterruptFabric;

pub(super) struct PreparedDevices {
    devices: AxVmDevices,
}

impl PreparedDevices {
    pub(super) fn build(
        vm: &AxVM,
        resources: &AxVMResources,
        factories: &DeviceFactoryRegistry,
        interrupt_fabric: &InterruptFabric,
    ) -> AxResult<Self> {
        let build_context = DeviceBuildContext::new(interrupt_fabric);
        let mut devices = AxVmDevices::build_with_factories(
            AxVmDeviceConfig {
                emu_configs: resources.config.emu_devices().to_vec(),
            },
            factories,
            &build_context,
        )?;

        #[cfg(target_arch = "x86_64")]
        for port in resources.config.pass_through_ports() {
            let passthrough = Arc::new(crate::host::x86_port::HostPortPassthrough::new(
                port.base,
                port.length,
            )?);
            let range = passthrough.address_range();
            debug!(
                "PT port region: [{:#x}~{:#x}]",
                range.start.number(),
                range.end.number(),
            );
            devices
                .register(PortDeviceAdapter::from_arc(passthrough))
                .map_err(|err| ax_err_type!(InvalidInput, format!("register PT port: {err:?}")))?;
        }

        Self::register_arch_devices(vm, resources, &mut devices)?;
        vm.add_special_emulated_devices(&mut devices)?;
        Ok(Self { devices })
    }

    pub(super) const fn devices(&self) -> &AxVmDevices {
        &self.devices
    }

    pub(super) fn into_inner(self) -> AxVmDevices {
        self.devices
    }

    #[cfg(target_arch = "aarch64")]
    fn register_arch_devices(
        vm: &AxVM,
        resources: &AxVMResources,
        devices: &mut AxVmDevices,
    ) -> AxResult {
        let passthrough = resources.config.interrupt_mode() == VMInterruptMode::Passthrough;
        if passthrough {
            let spis = resources.config.pass_through_spis();
            let cpu_id = vm.id() - 1; // FIXME: get the real CPU id.
            let mut gicd_found = false;

            for device in devices.devices() {
                if let Some(gicd) = device.as_any().downcast_ref::<arm_vgic::v3::vgicd::VGicD>() {
                    debug!("VGicD found, assigning SPIs...");

                    for spi in spis {
                        gicd.assign_irq(*spi + 32, cpu_id, (0, 0, 0, cpu_id as _))
                    }

                    gicd_found = true;
                    break;
                }
            }

            if !gicd_found {
                warn!("Failed to assign SPIs: No VGicD found in device list");
            }
        } else {
            for dev in axdevice::create_vtimer_devices() {
                devices
                    .register(Arc::from(dev) as Arc<dyn axdevice_base::Device>)
                    .map_err(|e| ax_err_type!(InvalidInput, format!("register vtimer: {e:?}")))?;
            }
        }
        Ok(())
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn register_arch_devices(
        _vm: &AxVM,
        _resources: &AxVMResources,
        _devices: &mut AxVmDevices,
    ) -> AxResult {
        Ok(())
    }
}

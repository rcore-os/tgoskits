//! Runtime factory for the machine-owned serial device.

use alloc::sync::Arc;

use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceDeclaration, DeviceFactory, DeviceFactoryRegistry,
    DeviceManagerError, DeviceManagerResult, DeviceRequirements, ResourceRequest, ResourceSlot,
    SerialBackend, build_16550_mmio, build_16550_port, build_pl011_mmio, validate_device_config,
};
use axdevice_base::{ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use super::{GuestSerialModel, GuestSerialProfile, GuestSerialTransport, serial_device_config};

pub(crate) fn register_machine_device_factories_from_config(
    config: &crate::config::AxVMConfig,
    factories: &mut DeviceFactoryRegistry,
) -> DeviceManagerResult {
    let profile = config.serial_profile();
    let backend_factory = config.serial_backend_factory();
    let backend = backend_factory.create();
    factories.register(Arc::new(MachineSerialFactory::new(profile, backend)))
}

struct MachineSerialFactory {
    profile: GuestSerialProfile,
    expected: EmulatedDeviceConfig,
    backend: Arc<dyn SerialBackend>,
}

impl MachineSerialFactory {
    fn new(profile: GuestSerialProfile, backend: Arc<dyn SerialBackend>) -> Self {
        Self {
            profile,
            expected: serial_device_config(profile),
            backend,
        }
    }

    fn validate_config(&self, config: &EmulatedDeviceConfig) -> DeviceManagerResult {
        validate_device_config(
            &self.expected,
            config,
            "build machine-owned virtual serial device",
        )
    }
}

impl DeviceFactory for MachineSerialFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::Console
    }

    fn declare(&self, config: &EmulatedDeviceConfig) -> DeviceManagerResult<DeviceDeclaration> {
        self.validate_config(config)?;
        let mut requirements = match self.profile.transport {
            GuestSerialTransport::Port { .. } => DeviceRequirements::new().with_pio(
                ResourceSlot::new("registers")?,
                u16::try_from(config.length).map_err(serial_declaration_range_error)?,
                1,
                ResourceRequest::Fixed(
                    u16::try_from(config.base_gpa).map_err(serial_declaration_range_error)?,
                ),
            )?,
            GuestSerialTransport::Mmio { .. } => DeviceRequirements::new().with_mmio(
                ResourceSlot::new("registers")?,
                u64::try_from(config.length).map_err(serial_declaration_range_error)?,
                1,
                ResourceRequest::Fixed(
                    u64::try_from(config.base_gpa).map_err(serial_declaration_range_error)?,
                ),
            )?,
        };
        requirements = requirements.with_wired_irq(
            ResourceSlot::new("irq")?,
            InterruptControllerId::new(0),
            InterruptTrigger::LevelTriggered,
            InterruptSharing::Exclusive,
            ResourceRequest::Fixed(ControllerInputId::new(config.irq_id)),
        )?;
        Ok(DeviceDeclaration::with_requirements(requirements))
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        self.validate_config(config)?;
        let irq = context.irq(&ResourceSlot::new("irq")?)?;
        let irq_id = irq.input().value();
        let bundle = match (self.profile.model, self.profile.transport) {
            (GuestSerialModel::Uart16550, GuestSerialTransport::Port { .. }) => {
                let (base, length) = context.pio(&ResourceSlot::new("registers")?)?;
                build_16550_port(base, length, irq_id, self.backend.clone(), irq)
            }
            (GuestSerialModel::Uart16550, GuestSerialTransport::Mmio { register_shift, .. }) => {
                let (base, length) = context.mmio(&ResourceSlot::new("registers")?)?;
                build_16550_mmio(
                    usize::try_from(base).map_err(serial_range_conversion_error)?,
                    usize::try_from(length).map_err(serial_range_conversion_error)?,
                    register_shift,
                    irq_id,
                    self.backend.clone(),
                    irq,
                )
            }
            (GuestSerialModel::Pl011, GuestSerialTransport::Mmio { .. }) => {
                let (base, length) = context.mmio(&ResourceSlot::new("registers")?)?;
                build_pl011_mmio(
                    usize::try_from(base).map_err(serial_range_conversion_error)?,
                    usize::try_from(length).map_err(serial_range_conversion_error)?,
                    irq_id,
                    self.backend.clone(),
                    irq,
                )
            }
            (GuestSerialModel::Pl011, GuestSerialTransport::Port { .. }) => {
                return Err(DeviceManagerError::Unsupported {
                    operation: "build machine-owned virtual serial device",
                    detail: "PL011 cannot use port I/O transport".into(),
                });
            }
        };
        Ok(bundle)
    }
}

fn serial_range_conversion_error(_error: core::num::TryFromIntError) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation: "build machine-owned virtual serial device",
        detail: "planned serial MMIO range exceeds the target address width".into(),
    }
}

fn serial_declaration_range_error(_error: core::num::TryFromIntError) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation: "declare machine-owned virtual serial resources",
        detail: "serial address or length exceeds the selected bus width".into(),
    }
}

//! Runtime factory for the machine-owned serial device.

use alloc::sync::Arc;

use axdevice::*;
use axdevice_base::*;

use super::*;

pub(crate) fn serial_device_model(config: &crate::config::AxVMConfig) -> Arc<dyn DeviceModel> {
    let profile = config.serial_profile();
    let backend_factory = config.serial_backend_factory();
    let backend = backend_factory.create();
    Arc::new(MachineSerialModel { profile, backend })
}

struct MachineSerialModel {
    profile: GuestSerialProfile,
    backend: Arc<dyn SerialBackend>,
}

impl DeviceModel for MachineSerialModel {
    fn declare(&self) -> DeviceManagerResult<DeviceDeclaration> {
        let mut requirements = match self.profile.transport {
            GuestSerialTransport::Port { base, length } => DeviceRequirements::new().with_pio(
                ResourceSlot::new("registers")?,
                length,
                1,
                ResourceRequest::Fixed(base),
            )?,
            GuestSerialTransport::Mmio { base, length, .. } => DeviceRequirements::new()
                .with_mmio(
                    ResourceSlot::new("registers")?,
                    u64::try_from(length).map_err(serial_declaration_range_error)?,
                    1,
                    ResourceRequest::Fixed(
                        u64::try_from(base).map_err(serial_declaration_range_error)?,
                    ),
                )?,
        };
        requirements = requirements.with_wired_irq(
            ResourceSlot::new("irq")?,
            InterruptControllerId::new(0),
            InterruptTrigger::LevelTriggered,
            InterruptSharing::Exclusive,
            ResourceRequest::Fixed(ControllerInputId::new(self.profile.irq)),
        )?;
        Ok(DeviceDeclaration::with_requirements(requirements))
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
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

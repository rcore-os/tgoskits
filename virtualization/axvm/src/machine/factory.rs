//! Configured constructors and runtime model for virtual serial devices.

use std::{string::ToString, sync::Arc};

use axdevice::*;
use axdevice_base::*;
use axvmconfig::VirtualDeviceRequest;

use super::*;
use crate::{ConfiguredDeviceError, ConfiguredModelRegistration, DeviceInstantiationContext};

const REGISTERS_SLOT: &str = "registers";
const IRQ_SLOT: &str = "irq";

pub(crate) const SERIAL_REGISTRATIONS: &[ConfiguredModelRegistration] = &[
    ConfiguredModelRegistration {
        model: "pl011-mmio",
        create: create_serial,
    },
    ConfiguredModelRegistration {
        model: "uart16550-mmio",
        create: create_serial,
    },
    ConfiguredModelRegistration {
        model: "uart16550-pio",
        create: create_serial,
    },
];

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerialOptions {
    clock_hz: Option<u32>,
    register_shift: Option<u8>,
    register_width: Option<u8>,
    backend: Option<SerialBackendOptions>,
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum SerialBackendOptions {
    HostConsole,
    Null,
}

pub(crate) fn model_name(profile: GuestSerialProfile) -> &'static str {
    match (profile.model, profile.transport) {
        (GuestSerialModel::Pl011, GuestSerialTransport::Mmio { .. }) => "pl011-mmio",
        (GuestSerialModel::Uart16550, GuestSerialTransport::Mmio { .. }) => "uart16550-mmio",
        (GuestSerialModel::Uart16550, GuestSerialTransport::Port { .. }) => "uart16550-pio",
        (GuestSerialModel::Pl011, GuestSerialTransport::Port { .. }) => "unsupported-pl011-pio",
    }
}

pub(crate) fn is_serial_model(model: &str) -> bool {
    SERIAL_REGISTRATIONS
        .iter()
        .any(|registration| registration.model == model)
}

fn create_serial(
    id: DeviceNodeId,
    request: &VirtualDeviceRequest,
    context: &DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
    let options = request
        .deserialize_options::<SerialOptions>()
        .map_err(|error| ConfiguredDeviceError::InvalidOptions {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: error.to_string(),
        })?;
    let controller =
        context
            .default_wired_controller()
            .ok_or_else(|| ConfiguredDeviceError::Instantiation {
                device: request.id.clone(),
                model: request.model.clone(),
                detail: "architecture has no default wired interrupt domain".into(),
            })?;
    let profile = configured_profile(request, options, context.serial_profile())?;
    let backend = match options.backend {
        Some(SerialBackendOptions::Null) => Arc::new(NullSerialBackend) as Arc<dyn SerialBackend>,
        Some(SerialBackendOptions::HostConsole) => context.serial_backend_factory().create(),
        None if context.host_console_by_default() => context.serial_backend_factory().create(),
        None => Arc::new(NullSerialBackend),
    };
    let model: Arc<dyn DeviceModel> = Arc::new(SerialDeviceModel {
        profile,
        controller,
        fixed: context.fixed_bindings().clone(),
        backend,
    });
    let mut node = if matches!(context.firmware_binding(), DeviceFirmwareBinding::None) {
        DeviceNodeSpec::virtual_device(id, model)
    } else {
        DeviceNodeSpec::host_replacement(id, model)
            .with_firmware_binding(context.firmware_binding().clone())
    };
    if let Some(controller_node) = context.default_wired_controller_node() {
        node = node.with_dependency(controller_node.clone());
    }
    Ok(node)
}

fn configured_profile(
    request: &VirtualDeviceRequest,
    options: SerialOptions,
    inherited: Option<GuestSerialProfile>,
) -> Result<GuestSerialProfile, ConfiguredDeviceError> {
    let mut profile = inherited
        .filter(|profile| model_name(*profile) == request.model)
        .unwrap_or_else(|| fallback_profile(&request.model));
    if let Some(clock_hz) = options.clock_hz {
        profile.clock_hz = clock_hz;
    }
    if let GuestSerialTransport::Mmio {
        register_shift,
        register_width,
        ..
    } = &mut profile.transport
    {
        if let Some(configured_shift) = options.register_shift {
            *register_shift = configured_shift;
        }
        if let Some(configured_width) = options.register_width {
            *register_width =
                AccessWidth::try_from(usize::from(configured_width)).map_err(|()| {
                    ConfiguredDeviceError::InvalidOptions {
                        device: request.id.clone(),
                        model: request.model.clone(),
                        detail: "register_width must be one of 1, 2, 4 or 8 bytes".into(),
                    }
                })?;
        }
    }
    Ok(profile)
}

pub(crate) fn fallback_profile(model: &str) -> GuestSerialProfile {
    match model {
        "pl011-mmio" => GuestSerialProfile {
            model: GuestSerialModel::Pl011,
            transport: GuestSerialTransport::Mmio {
                base: 0,
                length: 0x1000,
                register_shift: 0,
                register_width: AccessWidth::Dword,
            },
            irq: 0,
            clock_hz: 24_000_000,
        },
        "uart16550-pio" => GuestSerialProfile {
            model: GuestSerialModel::Uart16550,
            transport: GuestSerialTransport::Port { base: 0, length: 8 },
            irq: 0,
            clock_hz: 1_843_200,
        },
        _ => GuestSerialProfile {
            model: GuestSerialModel::Uart16550,
            transport: GuestSerialTransport::Mmio {
                base: 0,
                length: 0x100,
                register_shift: 0,
                register_width: AccessWidth::Byte,
            },
            irq: 0,
            clock_hz: 3_686_400,
        },
    }
}

struct SerialDeviceModel {
    profile: GuestSerialProfile,
    controller: InterruptControllerId,
    fixed: crate::FixedDeviceBindings,
    backend: Arc<dyn SerialBackend>,
}

impl DeviceModel for SerialDeviceModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        let registers = ResourceSlot::new(REGISTERS_SLOT)?;
        let irq = ResourceSlot::new(IRQ_SLOT)?;
        let mut requirements = match self.profile.transport {
            GuestSerialTransport::Port { length, .. } => DeviceRequirements::new().with_pio(
                registers.clone(),
                length,
                1,
                self.fixed
                    .pio(&registers)
                    .map_or(ResourceRequest::Auto, |(base, _)| {
                        ResourceRequest::Fixed(base)
                    }),
            )?,
            GuestSerialTransport::Mmio { length, .. } => DeviceRequirements::new().with_mmio(
                registers.clone(),
                u64::try_from(length).map_err(serial_declaration_range_error)?,
                1,
                self.fixed
                    .mmio(&registers)
                    .map_or(ResourceRequest::Auto, |(base, _)| {
                        ResourceRequest::Fixed(base)
                    }),
            )?,
        };
        let fixed_irq = self.fixed.wired(&irq);
        requirements = requirements.with_wired_irq(
            irq,
            fixed_irq.map_or(self.controller, |binding| binding.controller),
            fixed_irq.map_or(InterruptTrigger::LevelTriggered, |binding| binding.trigger),
            fixed_irq.map_or(InterruptSharing::Exclusive, |binding| binding.sharing),
            fixed_irq.map_or(ResourceRequest::Auto, |binding| {
                ResourceRequest::Fixed(binding.input)
            }),
        )?;
        Ok(requirements)
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        let mut spec = match self.profile.model {
            GuestSerialModel::Pl011 => DeviceFirmwareSpec::new("pl011")
                .with_compatible("arm,pl011")
                .with_acpi_hid("ARMH0011"),
            GuestSerialModel::Uart16550 => DeviceFirmwareSpec::new("serial")
                .with_compatible("ns16550a")
                .with_acpi_hid("PNP0501"),
        }
        .with_register(ResourceSlot::new(REGISTERS_SLOT).expect("static serial slot is valid"))
        .with_interrupt(ResourceSlot::new(IRQ_SLOT).expect("static serial slot is valid"))
        .with_u32_property("clock-frequency", self.profile.clock_hz);
        if let GuestSerialTransport::Mmio {
            register_shift,
            register_width,
            ..
        } = self.profile.transport
        {
            spec = spec
                .with_u32_property("reg-shift", u32::from(register_shift))
                .with_u32_property(
                    "reg-io-width",
                    u32::try_from(register_width.size())
                        .expect("an access width is at most eight bytes"),
                );
        }
        spec
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let irq = context.irq(IRQ_SLOT)?;
        let irq_id = irq.input().value();
        let bundle = match (self.profile.model, self.profile.transport) {
            (GuestSerialModel::Uart16550, GuestSerialTransport::Port { .. }) => {
                let (base, length) = context.pio(REGISTERS_SLOT)?;
                build_16550_port(base, length, irq_id, self.backend.clone(), irq)
            }
            (GuestSerialModel::Uart16550, GuestSerialTransport::Mmio { register_shift, .. }) => {
                let (base, length) = context.mmio(REGISTERS_SLOT)?;
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
                let (base, length) = context.mmio(REGISTERS_SLOT)?;
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
                    operation: "build virtual serial device",
                    detail: "PL011 cannot use port I/O transport".into(),
                });
            }
        };
        Ok(bundle)
    }
}

fn serial_range_conversion_error(_error: core::num::TryFromIntError) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation: "build virtual serial device",
        detail: "planned serial MMIO range exceeds the target address width".into(),
    }
}

fn serial_declaration_range_error(_error: core::num::TryFromIntError) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation: "declare virtual serial resources",
        detail: "serial address or length exceeds the selected bus width".into(),
    }
}

//! Configured constructors and runtime model for virtual serial devices.

use std::{string::ToString, sync::Arc};

use axdevice::*;
use axdevice_base::*;
use axvmconfig::VirtualDeviceRequest;

use super::*;
use crate::{
    ConfiguredDeviceError, ConfiguredModelRegistration, DeviceInstantiationContext,
    FixedDeviceBindings, FixedWiredBinding,
};

const REGISTERS_SLOT: &str = "registers";
const IRQ_SLOT: &str = "irq";

const SERIAL_MODELS: &[&str] = &["pl011-mmio", "uart16550-mmio", "uart16550-pio"];

pub(super) fn register_devices(
    catalog: &mut crate::ConfiguredDeviceCatalog,
) -> Result<(), ConfiguredDeviceError> {
    for model in SERIAL_MODELS {
        catalog.register(
            module_path!(),
            ConfiguredModelRegistration {
                model,
                create: create_serial,
            },
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerialOptions {
    base: Option<u64>,
    length: Option<u64>,
    irq: Option<usize>,
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
    SERIAL_MODELS.contains(&model)
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
    let fixed = configured_fixed_bindings(
        request,
        options,
        profile,
        context.fixed_bindings(),
        controller,
    )?;
    let backend = match options.backend {
        Some(SerialBackendOptions::Null) => Arc::new(NullSerialBackend) as Arc<dyn SerialBackend>,
        Some(SerialBackendOptions::HostConsole) => context.serial_backend_factory().create(),
        None if context.host_console_by_default() => context.serial_backend_factory().create(),
        None => Arc::new(NullSerialBackend),
    };
    let model: Arc<dyn DeviceModel> = Arc::new(SerialDeviceModel {
        profile,
        controller,
        fixed,
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

fn configured_fixed_bindings(
    request: &VirtualDeviceRequest,
    options: SerialOptions,
    profile: GuestSerialProfile,
    defaults: &FixedDeviceBindings,
    controller: InterruptControllerId,
) -> Result<FixedDeviceBindings, ConfiguredDeviceError> {
    let registers = ResourceSlot::new(REGISTERS_SLOT).map_err(|error| {
        ConfiguredDeviceError::Instantiation {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: error.to_string(),
        }
    })?;
    let irq =
        ResourceSlot::new(IRQ_SLOT).map_err(|error| ConfiguredDeviceError::Instantiation {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: error.to_string(),
        })?;
    let mut fixed = defaults.clone();
    if options.base.is_some() {
        fixed = match profile.transport {
            GuestSerialTransport::Port { base, length } => fixed.with_pio(registers, base, length),
            GuestSerialTransport::Mmio { base, length, .. } => fixed.with_mmio(
                registers,
                u64::try_from(base).map_err(|_| serial_option_error(request, "base"))?,
                u64::try_from(length).map_err(|_| serial_option_error(request, "length"))?,
            ),
        };
    }
    if let Some(input) = options.irq {
        let controller = defaults
            .wired(&irq)
            .map_or(controller, |binding| binding.controller);
        fixed = fixed.with_wired(
            irq,
            FixedWiredBinding {
                controller,
                input: ControllerInputId::new(input),
                trigger: InterruptTrigger::LevelTriggered,
                sharing: InterruptSharing::Exclusive,
            },
        );
    }
    Ok(fixed)
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
    match &mut profile.transport {
        GuestSerialTransport::Port { base, length } => {
            if let Some(configured_base) = options.base {
                *base = u16::try_from(configured_base)
                    .map_err(|_| serial_option_error(request, "base"))?;
            }
            if let Some(configured_length) = options.length {
                *length = u16::try_from(configured_length)
                    .map_err(|_| serial_option_error(request, "length"))?;
            }
        }
        GuestSerialTransport::Mmio {
            base,
            length,
            register_shift,
            register_width,
        } => {
            if let Some(configured_base) = options.base {
                *base = usize::try_from(configured_base)
                    .map_err(|_| serial_option_error(request, "base"))?;
            }
            if let Some(configured_length) = options.length {
                *length = usize::try_from(configured_length)
                    .map_err(|_| serial_option_error(request, "length"))?;
            }
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
    }
    Ok(profile)
}

fn serial_option_error(
    request: &VirtualDeviceRequest,
    option: &'static str,
) -> ConfiguredDeviceError {
    ConfiguredDeviceError::InvalidOptions {
        device: request.id.clone(),
        model: request.model.clone(),
        detail: std::format!("{option} does not fit the selected serial transport"),
    }
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
        let registers = ResourceSlot::new(REGISTERS_SLOT).expect("static serial slot is valid");
        let interrupt = ResourceSlot::new(IRQ_SLOT).expect("static serial slot is valid");
        let (mut fdt, acpi) = match self.profile.model {
            GuestSerialModel::Pl011 => (
                FdtNodeSpec::new("pl011").with_compatible("arm,pl011"),
                AcpiDeviceSpec::new("COM0", "ARMH0011"),
            ),
            GuestSerialModel::Uart16550 => (
                FdtNodeSpec::new("serial").with_compatible("ns16550a"),
                AcpiDeviceSpec::new("COM0", "PNP0501"),
            ),
        };
        fdt = fdt
            .with_register(registers.clone())
            .with_interrupt(interrupt.clone())
            .with_u32_property("clock-frequency", self.profile.clock_hz);
        if let GuestSerialTransport::Mmio {
            register_shift,
            register_width,
            ..
        } = self.profile.transport
        {
            fdt = fdt
                .with_u32_property("reg-shift", u32::from(register_shift))
                .with_u32_property(
                    "reg-io-width",
                    u32::try_from(register_width.size())
                        .expect("an access width is at most eight bytes"),
                );
        }
        DeviceFirmwareSpec::interfaces(
            Some(std::vec![FdtContributionSpec::Console(fdt)]),
            Some(std::vec![AcpiContributionSpec::Console(
                acpi.with_register(registers)
                    .with_interrupt(interrupt)
                    .with_u32_property("clock-frequency", self.profile.clock_hz),
            )]),
        )
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

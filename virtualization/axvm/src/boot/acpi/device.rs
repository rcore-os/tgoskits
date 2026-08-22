//! Generic ACPI devices resolved from virtual-device firmware contributions.

use std::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    vec::Vec,
};

use acpi_tables::{
    Aml, AmlSink,
    aml::{Device, IO, Interrupt, Memory32Fixed, Name, ResourceTemplate},
};
use axdevice::{
    AcpiContributionSpec, AcpiDeviceSpec, DeviceFirmwareProperty, ResolvedDeviceGraph,
    ResolvedDeviceResources,
};
use axdevice_base::{InterruptControllerId, InterruptSharing, InterruptTrigger};

use super::AcpiBuildError;
use crate::{AxVmError, AxVmResult};

/// One ACPI register resource resolved from a graph slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedAcpiRegister {
    Mmio { base: u64, size: u64 },
    Pio { base: u16, size: u16 },
}

/// One ACPI interrupt resource resolved from a graph slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedAcpiInterrupt {
    pub(crate) controller: InterruptControllerId,
    pub(crate) input: u32,
    pub(crate) trigger: InterruptTrigger,
    pub(crate) sharing: InterruptSharing,
}

/// One conventional AML device with all graph slots resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedAcpiDevice {
    pub(crate) name: String,
    pub(crate) hid: String,
    pub(crate) uid: u32,
    pub(crate) registers: Vec<ResolvedAcpiRegister>,
    pub(crate) interrupts: Vec<ResolvedAcpiInterrupt>,
}

/// One ACPI property with graph references resolved to typed values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedAcpiProperty {
    Empty(String),
    U32(String, u32),
    String(String, String),
    InterruptInput {
        name: String,
        interrupt: ResolvedAcpiInterrupt,
    },
}

/// Architecture-owned ACPI topology category with resolved controller identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedAcpiSpecialKind {
    InterruptController(InterruptControllerId),
    Timer,
    PciHostBridge,
    Console,
    FirmwareTransport,
}

/// One architecture-owned ACPI contribution with every resource slot resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedAcpiSpecial {
    pub(crate) id: String,
    pub(crate) kind: ResolvedAcpiSpecialKind,
    pub(crate) name: String,
    pub(crate) hid: Option<String>,
    pub(crate) registers: Vec<ResolvedAcpiRegister>,
    pub(crate) interrupts: Vec<ResolvedAcpiInterrupt>,
    pub(crate) properties: Vec<ResolvedAcpiProperty>,
}

/// Complete ACPI contribution plan selected from one resolved device graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedAcpiFirmware {
    pub(crate) devices: Vec<ResolvedAcpiDevice>,
    pub(crate) specials: Vec<ResolvedAcpiSpecial>,
}

/// Maps one runtime interrupt-controller namespace into the ACPI GSI namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AcpiInterruptControllerMap {
    controller: InterruptControllerId,
    gsi_base: u32,
}

impl AcpiInterruptControllerMap {
    pub(crate) const fn new(controller: InterruptControllerId, gsi_base: u32) -> Self {
        Self {
            controller,
            gsi_base,
        }
    }
}

/// Resolves conventional ACPI contributions without inspecting model names.
#[cfg(test)]
pub(crate) fn resolve_devices(graph: &ResolvedDeviceGraph) -> AxVmResult<Vec<ResolvedAcpiDevice>> {
    Ok(resolve_acpi_firmware(graph)?.devices)
}

/// Resolves every ACPI contribution, including architecture-owned topology.
pub(crate) fn resolve_acpi_firmware(
    graph: &ResolvedDeviceGraph,
) -> AxVmResult<ResolvedAcpiFirmware> {
    graph
        .validate_acpi_support()
        .map_err(|error| AxVmError::invalid_config(std::format!("{error}")))?;
    let mut devices = Vec::new();
    let mut specials = Vec::new();
    let mut allocated_names = BTreeSet::new();
    let mut next_name_index = BTreeMap::<String, u32>::new();
    let mut next_hid_uid = BTreeMap::<String, u32>::new();
    for graph_node in graph.nodes() {
        let Some(contributions) = graph_node.firmware().acpi() else {
            continue;
        };
        let resources = graph.resources_for(graph_node.id())?;
        for contribution in contributions {
            let (kind, device) = match contribution {
                AcpiContributionSpec::Conventional(device) => (None, device),
                AcpiContributionSpec::InterruptController { controller, device } => (
                    Some(ResolvedAcpiSpecialKind::InterruptController(*controller)),
                    device,
                ),
                AcpiContributionSpec::Timer(device) => {
                    (Some(ResolvedAcpiSpecialKind::Timer), device)
                }
                AcpiContributionSpec::PciHostBridge(device) => {
                    (Some(ResolvedAcpiSpecialKind::PciHostBridge), device)
                }
                AcpiContributionSpec::Console(device) => {
                    (Some(ResolvedAcpiSpecialKind::Console), device)
                }
                AcpiContributionSpec::FirmwareTransport(device) => {
                    (Some(ResolvedAcpiSpecialKind::FirmwareTransport), device)
                }
            };
            let registers = resolve_registers(resources, device)?;
            let interrupts = resolve_interrupts(graph_node.id().as_str(), resources, device)?;
            let properties = resolve_properties(graph_node.id().as_str(), resources, device)?;
            if let Some(kind) = kind {
                specials.push(ResolvedAcpiSpecial {
                    id: graph_node.id().to_string(),
                    kind,
                    name: device.name().into(),
                    hid: device.hid().map(Into::into),
                    registers,
                    interrupts,
                    properties,
                });
                continue;
            }
            let hid = device.hid().ok_or_else(|| {
                AxVmError::invalid_config(std::format!(
                    "conventional ACPI device {} has no hardware identifier",
                    graph_node.id()
                ))
            })?;
            let name = if device.has_indexed_name() {
                let index = next_name_index.entry(device.name().into()).or_default();
                let name = indexed_acpi_name(device.name(), *index).ok_or_else(|| {
                    AxVmError::invalid_config(std::format!(
                        "device {} has invalid or exhausted ACPI name prefix '{}'",
                        graph_node.id(),
                        device.name()
                    ))
                })?;
                *index = index.checked_add(1).ok_or_else(|| {
                    AxVmError::invalid_config("ACPI instance-name index overflowed")
                })?;
                name
            } else {
                validate_fixed_acpi_name(graph_node.id().as_str(), device.name())?;
                device.name().into()
            };
            if !allocated_names.insert(name.clone()) {
                return Err(AxVmError::invalid_config(std::format!(
                    "device {} contributes duplicate ACPI name '{name}'",
                    graph_node.id()
                )));
            }
            let uid = next_hid_uid.entry(hid.into()).or_default();
            let assigned_uid = *uid;
            *uid = uid
                .checked_add(1)
                .ok_or_else(|| AxVmError::invalid_config("ACPI device UID overflowed"))?;
            if !properties.is_empty() {
                return Err(AxVmError::unsupported(
                    "resolve conventional ACPI device properties",
                    std::format!(
                        "device {} declares ACPI properties that have no typed encoder",
                        graph_node.id()
                    ),
                ));
            }
            devices.push(ResolvedAcpiDevice {
                name,
                hid: hid.into(),
                uid: assigned_uid,
                registers,
                interrupts,
            });
        }
    }
    Ok(ResolvedAcpiFirmware { devices, specials })
}

fn resolve_registers(
    resources: &ResolvedDeviceResources,
    device: &AcpiDeviceSpec,
) -> AxVmResult<Vec<ResolvedAcpiRegister>> {
    device
        .register_slots()
        .iter()
        .map(|slot| {
            if let Ok((base, size)) = resources.mmio(slot) {
                Ok(ResolvedAcpiRegister::Mmio { base, size })
            } else {
                resources
                    .pio(slot)
                    .map(|(base, size)| ResolvedAcpiRegister::Pio { base, size })
                    .map_err(Into::into)
            }
        })
        .collect()
}

fn resolve_interrupts(
    device_id: &str,
    resources: &ResolvedDeviceResources,
    device: &AcpiDeviceSpec,
) -> AxVmResult<Vec<ResolvedAcpiInterrupt>> {
    device
        .interrupt_slots()
        .iter()
        .map(|slot| {
            let irq = resources.wired_irq(slot)?;
            Ok(ResolvedAcpiInterrupt {
                controller: irq.controller(),
                input: u32::try_from(irq.input().value()).map_err(|_| {
                    AxVmError::invalid_config(std::format!(
                        "device {device_id} interrupt exceeds ACPI GSI width"
                    ))
                })?,
                trigger: irq.trigger(),
                sharing: irq.sharing(),
            })
        })
        .collect()
}

fn resolve_properties(
    device_id: &str,
    resources: &ResolvedDeviceResources,
    device: &AcpiDeviceSpec,
) -> AxVmResult<Vec<ResolvedAcpiProperty>> {
    device
        .properties()
        .iter()
        .map(|property| match property {
            DeviceFirmwareProperty::Empty { name } => Ok(ResolvedAcpiProperty::Empty(name.clone())),
            DeviceFirmwareProperty::U32 { name, value } => {
                Ok(ResolvedAcpiProperty::U32(name.clone(), *value))
            }
            DeviceFirmwareProperty::String { name, value } => {
                Ok(ResolvedAcpiProperty::String(name.clone(), value.clone()))
            }
            DeviceFirmwareProperty::InterruptInput { name, slot } => {
                let irq = resources.wired_irq(slot).map_err(|error| {
                    AxVmError::invalid_config(std::format!(
                        "device {device_id} has an invalid ACPI interrupt property: {error}"
                    ))
                })?;
                Ok(ResolvedAcpiProperty::InterruptInput {
                    name: name.clone(),
                    interrupt: ResolvedAcpiInterrupt {
                        controller: irq.controller(),
                        input: u32::try_from(irq.input().value()).map_err(|_| {
                            AxVmError::invalid_config(std::format!(
                                "device {device_id} ACPI interrupt property exceeds GSI width"
                            ))
                        })?,
                        trigger: irq.trigger(),
                        sharing: irq.sharing(),
                    },
                })
            }
        })
        .collect()
}

/// Encodes generic devices as AML under `\_SB`.
#[cfg(test)]
pub(crate) fn encode_devices(devices: &[ResolvedAcpiDevice]) -> Result<Vec<u8>, AcpiBuildError> {
    encode_devices_with_interrupt_controllers(
        devices,
        &[AcpiInterruptControllerMap::new(
            InterruptControllerId::new(0),
            0,
        )],
    )
}

/// Encodes generic devices after mapping runtime controller inputs to ACPI GSIs.
pub(crate) fn encode_devices_with_interrupt_controllers(
    devices: &[ResolvedAcpiDevice],
    controllers: &[AcpiInterruptControllerMap],
) -> Result<Vec<u8>, AcpiBuildError> {
    let mut aml = Vec::new();
    for device in devices {
        let hid = Name::new("_HID".into(), &device.hid);
        let uid = Name::new("_UID".into(), &device.uid);
        let resources = device
            .registers
            .iter()
            .copied()
            .map(aml_register)
            .chain(
                device
                    .interrupts
                    .iter()
                    .copied()
                    .map(|interrupt| aml_interrupt(interrupt, controllers)),
            )
            .collect::<Result<Vec<_>, _>>()?;
        let resource_refs = resources
            .iter()
            .map(|resource| resource as &dyn Aml)
            .collect::<Vec<_>>();
        let template = ResourceTemplate::new(resource_refs);
        let crs = Name::new("_CRS".into(), &template);
        Device::new(
            std::format!("_SB_.{}", device.name).as_str().into(),
            std::vec![&hid, &uid, &crs],
        )
        .to_aml_bytes(&mut aml);
    }
    Ok(aml)
}

fn validate_fixed_acpi_name(device_id: &str, name: &str) -> AxVmResult {
    if name.len() == 4
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Ok(());
    }
    Err(AxVmError::invalid_config(std::format!(
        "device {device_id} has invalid four-character ACPI name '{name}'"
    )))
}

fn indexed_acpi_name(prefix: &str, index: u32) -> Option<String> {
    if prefix.is_empty()
        || prefix.len() >= 4
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return None;
    }
    let suffix_width = 4 - prefix.len();
    let capacity = 36u32.checked_pow(suffix_width as u32)?;
    if index >= capacity {
        return None;
    }
    let mut suffix = std::vec![b'0'; suffix_width];
    let mut remaining = index;
    for byte in suffix.iter_mut().rev() {
        let digit = (remaining % 36) as u8;
        *byte = if digit < 10 {
            b'0' + digit
        } else {
            b'A' + digit - 10
        };
        remaining /= 36;
    }
    let mut name = prefix.as_bytes().to_vec();
    name.extend_from_slice(&suffix);
    String::from_utf8(name).ok()
}

enum AmlResource {
    Mmio(Memory32Fixed),
    Pio(IO),
    Interrupt(Interrupt),
}

impl Aml for AmlResource {
    fn to_aml_bytes(&self, bytes: &mut dyn AmlSink) {
        match self {
            Self::Mmio(resource) => resource.to_aml_bytes(bytes),
            Self::Pio(resource) => resource.to_aml_bytes(bytes),
            Self::Interrupt(resource) => resource.to_aml_bytes(bytes),
        }
    }
}

fn aml_register(register: ResolvedAcpiRegister) -> Result<AmlResource, AcpiBuildError> {
    match register {
        ResolvedAcpiRegister::Mmio { base, size } => Ok(AmlResource::Mmio(Memory32Fixed::new(
            true,
            u32::try_from(base).map_err(|_| AcpiBuildError::InvalidValue {
                field: "configured ACPI MMIO base",
                value: std::format!("{base:#x}"),
            })?,
            u32::try_from(size).map_err(|_| AcpiBuildError::InvalidValue {
                field: "configured ACPI MMIO size",
                value: std::format!("{size:#x}"),
            })?,
        ))),
        ResolvedAcpiRegister::Pio { base, size } => Ok(AmlResource::Pio(IO::new(
            base,
            base,
            0,
            u8::try_from(size).map_err(|_| AcpiBuildError::InvalidValue {
                field: "configured ACPI PIO size",
                value: size.to_string(),
            })?,
        ))),
    }
}

fn aml_interrupt(
    interrupt: ResolvedAcpiInterrupt,
    controllers: &[AcpiInterruptControllerMap],
) -> Result<AmlResource, AcpiBuildError> {
    let gsi = acpi_gsi(interrupt, controllers)?;
    Ok(AmlResource::Interrupt(Interrupt::new(
        true,
        interrupt.trigger == InterruptTrigger::EdgeTriggered,
        false,
        interrupt.sharing == InterruptSharing::Shared,
        gsi,
    )))
}

fn acpi_gsi(
    interrupt: ResolvedAcpiInterrupt,
    controllers: &[AcpiInterruptControllerMap],
) -> Result<u32, AcpiBuildError> {
    let mapping = controllers
        .iter()
        .find(|mapping| mapping.controller == interrupt.controller)
        .ok_or_else(|| AcpiBuildError::InvalidValue {
            field: "configured ACPI interrupt controller",
            value: interrupt.controller.value().to_string(),
        })?;
    mapping
        .gsi_base
        .checked_add(interrupt.input)
        .ok_or_else(|| AcpiBuildError::InvalidValue {
            field: "configured ACPI GSI",
            value: std::format!("{} + {}", mapping.gsi_base, interrupt.input),
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axdevice::*;
    use axdevice_base::{
        ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger,
    };

    use super::{
        AcpiInterruptControllerMap, ResolvedAcpiInterrupt, acpi_gsi, encode_devices,
        indexed_acpi_name, resolve_devices,
    };

    enum RegressionModel {
        InvalidTimerSlot,
        ConventionalOnSecondController,
    }

    impl DeviceModel for RegressionModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            match self {
                Self::InvalidTimerSlot => DeviceRequirements::new().with_pio(
                    ResourceSlot::new("registers")?,
                    4,
                    1,
                    ResourceRequest::Auto,
                ),
                Self::ConventionalOnSecondController => DeviceRequirements::new().with_wired_irq(
                    ResourceSlot::new("irq")?,
                    InterruptControllerId::new(1),
                    InterruptTrigger::LevelTriggered,
                    InterruptSharing::Exclusive,
                    ResourceRequest::Auto,
                ),
            }
        }

        fn firmware(&self) -> DeviceFirmwareSpec {
            let contribution = match self {
                Self::InvalidTimerSlot => AcpiContributionSpec::Timer(
                    AcpiDeviceSpec::new("TMR0", "ACPI0008").with_register(
                        ResourceSlot::new("misspelled-registers")
                            .expect("static regression slot is valid"),
                    ),
                ),
                Self::ConventionalOnSecondController => AcpiContributionSpec::Conventional(
                    AcpiDeviceSpec::new("DEV0", "TEST0001").with_interrupt(
                        ResourceSlot::new("irq").expect("static regression slot is valid"),
                    ),
                ),
            };
            DeviceFirmwareSpec::interfaces(None, Some(std::vec![contribution]))
        }

        fn build(
            &self,
            _context: &mut DeviceBuildContext<'_>,
        ) -> DeviceManagerResult<DeviceBundle> {
            unreachable!("firmware-resolution regression does not build devices")
        }
    }

    fn resolve_model(model: RegressionModel, pools: ResourcePools) -> ResolvedDeviceGraph {
        let mut builder = DeviceGraphBuilder::new();
        builder
            .add(DeviceNodeSpec::virtual_device(
                DeviceNodeId::new("regression-device").unwrap(),
                Arc::new(model),
            ))
            .unwrap();
        builder.declare().unwrap().resolve(pools).unwrap()
    }

    #[test]
    fn indexed_names_are_unique_and_bounded_name_segments() {
        assert_eq!(indexed_acpi_name("VB", 0).as_deref(), Some("VB00"));
        assert_eq!(indexed_acpi_name("VB", 35).as_deref(), Some("VB0Z"));
        assert_eq!(indexed_acpi_name("VB", 36).as_deref(), Some("VB10"));
        assert_eq!(indexed_acpi_name("VB", 1296), None);
    }

    #[test]
    fn special_acpi_contribution_rejects_unknown_resource_slot() {
        let mut pools = ResourcePools::new();
        pools.add_auto_pio(0x1000..0x1100).unwrap();
        let graph = resolve_model(RegressionModel::InvalidTimerSlot, pools);

        assert!(resolve_devices(&graph).is_err());
    }

    #[test]
    fn generic_acpi_encoder_rejects_unmapped_interrupt_controller() {
        let mut pools = ResourcePools::new();
        pools
            .add_auto_controller_inputs(
                InterruptControllerId::new(1),
                ControllerInputId::new(32)..ControllerInputId::new(33),
            )
            .unwrap();
        let graph = resolve_model(RegressionModel::ConventionalOnSecondController, pools);
        let devices = resolve_devices(&graph).unwrap();

        assert!(encode_devices(&devices).is_err());
    }

    #[test]
    fn acpi_interrupt_controller_map_translates_input_to_gsi() {
        let interrupt = ResolvedAcpiInterrupt {
            controller: InterruptControllerId::new(3),
            input: 48,
            trigger: InterruptTrigger::LevelTriggered,
            sharing: InterruptSharing::Exclusive,
        };

        assert_eq!(
            acpi_gsi(
                interrupt,
                &[AcpiInterruptControllerMap::new(
                    InterruptControllerId::new(3),
                    64,
                )],
            )
            .unwrap(),
            112
        );
    }
}

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
use axdevice::{AcpiContributionSpec, ResolvedDeviceGraph};
use axdevice_base::{InterruptSharing, InterruptTrigger};

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
    input: u32,
    trigger: InterruptTrigger,
    sharing: InterruptSharing,
}

/// One conventional AML device with all graph slots resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedAcpiDevice {
    name: String,
    hid: String,
    uid: u32,
    registers: Vec<ResolvedAcpiRegister>,
    interrupts: Vec<ResolvedAcpiInterrupt>,
}

/// Resolves conventional ACPI contributions without inspecting model names.
pub(crate) fn resolve_devices(graph: &ResolvedDeviceGraph) -> AxVmResult<Vec<ResolvedAcpiDevice>> {
    graph
        .validate_acpi_support()
        .map_err(|error| AxVmError::invalid_config(std::format!("{error}")))?;
    let mut devices = Vec::new();
    let mut allocated_names = BTreeSet::new();
    let mut next_name_index = BTreeMap::<String, u32>::new();
    let mut next_hid_uid = BTreeMap::<String, u32>::new();
    for graph_node in graph.nodes() {
        let Some(contributions) = graph_node.firmware().acpi() else {
            continue;
        };
        let resources = graph.resources_for(graph_node.id())?;
        for contribution in contributions {
            let AcpiContributionSpec::Conventional(device) = contribution else {
                continue;
            };
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
            if !device.properties().is_empty() {
                return Err(AxVmError::unsupported(
                    "resolve conventional ACPI device properties",
                    std::format!(
                        "device {} declares ACPI properties that have no typed encoder",
                        graph_node.id()
                    ),
                ));
            }
            let registers = device
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
                .collect::<AxVmResult<Vec<_>>>()?;
            let interrupts = device
                .interrupt_slots()
                .iter()
                .map(|slot| {
                    let irq = resources.wired_irq(slot)?;
                    Ok(ResolvedAcpiInterrupt {
                        input: u32::try_from(irq.input().value()).map_err(|_| {
                            AxVmError::invalid_config(std::format!(
                                "device {} interrupt exceeds ACPI GSI width",
                                graph_node.id()
                            ))
                        })?,
                        trigger: irq.trigger(),
                        sharing: irq.sharing(),
                    })
                })
                .collect::<AxVmResult<Vec<_>>>()?;
            devices.push(ResolvedAcpiDevice {
                name,
                hid: hid.into(),
                uid: assigned_uid,
                registers,
                interrupts,
            });
        }
    }
    Ok(devices)
}

/// Encodes generic devices as AML under `\_SB`.
pub(crate) fn encode_devices(devices: &[ResolvedAcpiDevice]) -> Result<Vec<u8>, AcpiBuildError> {
    let mut aml = Vec::new();
    for device in devices {
        let hid = Name::new("_HID".into(), &device.hid);
        let uid = Name::new("_UID".into(), &device.uid);
        let resources = device
            .registers
            .iter()
            .copied()
            .map(aml_register)
            .chain(device.interrupts.iter().copied().map(aml_interrupt))
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

fn aml_interrupt(interrupt: ResolvedAcpiInterrupt) -> Result<AmlResource, AcpiBuildError> {
    Ok(AmlResource::Interrupt(Interrupt::new(
        true,
        interrupt.trigger == InterruptTrigger::EdgeTriggered,
        false,
        interrupt.sharing == InterruptSharing::Shared,
        interrupt.input,
    )))
}

#[cfg(test)]
mod tests {
    use super::indexed_acpi_name;

    #[test]
    fn indexed_names_are_unique_and_bounded_name_segments() {
        assert_eq!(indexed_acpi_name("VB", 0).as_deref(), Some("VB00"));
        assert_eq!(indexed_acpi_name("VB", 35).as_deref(), Some("VB0Z"));
        assert_eq!(indexed_acpi_name("VB", 36).as_deref(), Some("VB10"));
        assert_eq!(indexed_acpi_name("VB", 1296), None);
    }
}

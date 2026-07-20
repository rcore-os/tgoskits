//! Discovery and validation of AArch64 architectural interrupt roles.

use alloc::{collections::BTreeSet, format, vec::Vec};

use arm_vgic::{IntId, PpiId};
use fdt_edit::{Fdt, Status};

use crate::{AxVmError, AxVmResult};

const DEFAULT_GIC_MAINTENANCE_INTID: u32 = 25;
const DEFAULT_GUEST_PHYSICAL_TIMER_INTID: u32 = 30;
const GIC_INTERRUPT_CELLS: usize = 3;

/// VM-internal classification of host-reserved and guest timer interrupts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Aarch64InterruptRoles {
    host_reserved: BTreeSet<IntId>,
    maintenance: PpiId,
    guest_timer: PpiId,
}

/// Platform and firmware capabilities used to derive one VM's interrupt roles.
pub(crate) struct Aarch64InterruptDiscovery<'a> {
    pub(crate) host_ipi_intid: u32,
    pub(crate) host_timer_intid: u32,
    pub(crate) host_fdt_bytes: Option<&'a [u8]>,
    pub(crate) guest_fdt_bytes: Option<&'a [u8]>,
    pub(crate) passthrough_intids: &'a [u32],
}

struct DiscoveredInterruptIds<'a> {
    host_ipi: u32,
    host_timer: u32,
    maintenance: u32,
    guest_timer: PpiId,
    passthrough: &'a [u32],
}

impl Aarch64InterruptRoles {
    pub(crate) fn discover(discovery: Aarch64InterruptDiscovery<'_>) -> AxVmResult<Self> {
        let maintenance = match discovery.host_fdt_bytes {
            Some(bytes) => discover_maintenance_intid(bytes)?,
            None => DEFAULT_GIC_MAINTENANCE_INTID,
        };
        let guest_timer = match discovery.guest_fdt_bytes {
            Some(bytes) => discover_guest_physical_timer_ppi(bytes)?,
            None => default_guest_physical_timer_ppi()?,
        };
        Self::from_discovered_intids(DiscoveredInterruptIds {
            host_ipi: discovery.host_ipi_intid,
            host_timer: discovery.host_timer_intid,
            maintenance,
            guest_timer,
            passthrough: discovery.passthrough_intids,
        })
    }

    pub(crate) fn host_reserved(&self) -> &BTreeSet<IntId> {
        &self.host_reserved
    }

    /// Returns the host PPI used by ICH maintenance conditions.
    pub(crate) const fn maintenance_interrupt(&self) -> PpiId {
        self.maintenance
    }

    /// Returns the PPI driven by the emulated EL1 physical timer.
    pub(crate) const fn guest_physical_timer(&self) -> PpiId {
        self.guest_timer
    }

    fn from_discovered_intids(discovered: DiscoveredInterruptIds<'_>) -> AxVmResult<Self> {
        let DiscoveredInterruptIds {
            host_ipi,
            host_timer,
            maintenance,
            guest_timer,
            passthrough,
        } = discovered;
        let maintenance = checked_ppi("GIC maintenance", maintenance)?;
        let mut host_reserved = BTreeSet::new();
        for (role, intid) in [
            ("host IPI", checked_core_intid("host IPI", host_ipi)?),
            ("host timer", checked_core_intid("host timer", host_timer)?),
            ("GIC maintenance", IntId::Ppi(maintenance)),
        ] {
            if !host_reserved.insert(intid) {
                return Err(AxVmError::invalid_config(format!(
                    "internally discovered {role} reuses core INTID {}",
                    intid.raw()
                )));
            }
        }

        let guest_timer_intid = IntId::Ppi(guest_timer);
        if host_reserved.contains(&guest_timer_intid) {
            return Err(AxVmError::invalid_config(format!(
                "guest timer INTID {} conflicts with an internally discovered host interrupt",
                guest_timer_intid.raw()
            )));
        }

        for raw in passthrough.iter().copied().collect::<BTreeSet<_>>() {
            let intid = IntId::new(raw).map_err(|error| {
                AxVmError::invalid_config(format!(
                    "passthrough device INTID {raw} is invalid: {error}"
                ))
            })?;
            if host_reserved.contains(&intid) {
                return Err(AxVmError::invalid_config(format!(
                    "passthrough device INTID {raw} conflicts with an internally reserved host \
                     interrupt"
                )));
            }
            if u32::from(guest_timer.raw()) == raw {
                return Err(AxVmError::invalid_config(format!(
                    "passthrough device INTID {raw} conflicts with a guest timer"
                )));
            }
        }

        Ok(Self {
            host_reserved,
            maintenance,
            guest_timer,
        })
    }
}

fn checked_core_intid(role: &'static str, raw: u32) -> AxVmResult<IntId> {
    IntId::new(raw).map_err(|error| {
        AxVmError::invalid_config(format!("{role} reports invalid GIC INTID {raw}: {error}"))
    })
}

fn checked_ppi(role: &'static str, raw: u32) -> AxVmResult<PpiId> {
    let raw = u8::try_from(raw)
        .map_err(|_| AxVmError::invalid_config(format!("{role} INTID {raw} is not a PPI")))?;
    PpiId::new(raw)
        .map_err(|error| AxVmError::invalid_config(format!("{role} is invalid: {error}")))
}

fn discover_maintenance_intid(bytes: &[u8]) -> AxVmResult<u32> {
    let fdt = parse_fdt(bytes, "host")?;
    let Some(gic) = fdt
        .find_compatible(&["arm,gic-v3"])
        .into_iter()
        .find(|node| node.as_node().status() != Some(Status::Disabled))
    else {
        return Ok(DEFAULT_GIC_MAINTENANCE_INTID);
    };
    let Some(interrupts) = gic.as_node().get_property("interrupts") else {
        return Ok(DEFAULT_GIC_MAINTENANCE_INTID);
    };
    let cells = interrupts.get_u32_iter().collect::<Vec<_>>();
    let first = cells.get(..GIC_INTERRUPT_CELLS).ok_or_else(|| {
        AxVmError::invalid_config("host GICv3 maintenance interrupt specifier is truncated")
    })?;
    decode_gic_ppi(first, "host GICv3 maintenance interrupt").map(|ppi| u32::from(ppi.raw()))
}

fn discover_guest_physical_timer_ppi(bytes: &[u8]) -> AxVmResult<PpiId> {
    let fdt = parse_fdt(bytes, "guest")?;
    let Some(timer) = fdt
        .find_compatible(&["arm,armv8-timer"])
        .into_iter()
        .find(|node| node.as_node().status() != Some(Status::Disabled))
    else {
        return default_guest_physical_timer_ppi();
    };
    let interrupts = timer
        .as_node()
        .get_property("interrupts")
        .ok_or_else(|| AxVmError::invalid_config("guest Arm timer has no interrupts property"))?;
    let cells = interrupts.get_u32_iter().collect::<Vec<_>>();
    let (entries, remainder) = cells.as_chunks::<GIC_INTERRUPT_CELLS>();
    if !remainder.is_empty() {
        return Err(AxVmError::invalid_config(format!(
            "guest Arm timer interrupt property has {} cells, not complete GIC specifiers",
            cells.len()
        )));
    }
    let names = timer
        .as_node()
        .get_property("interrupt-names")
        .map(|property| property.as_str_iter().collect::<Vec<_>>());
    select_guest_physical_timer_ppi(entries, names.as_deref())
}

fn select_guest_physical_timer_ppi(
    entries: &[[u32; GIC_INTERRUPT_CELLS]],
    names: Option<&[&str]>,
) -> AxVmResult<PpiId> {
    let physical = if let Some(names) = names {
        if names.len() != entries.len() {
            return Err(AxVmError::invalid_config(
                "guest Arm timer interrupt-names count does not match interrupts",
            ));
        }
        let physical = names
            .iter()
            .position(|name| matches!(*name, "phys" | "non-secure-phys" | "nonsecure-phys"))
            .ok_or_else(|| {
                AxVmError::invalid_config("guest Arm timer has no non-secure physical timer IRQ")
            })?;
        entries[physical].as_slice()
    } else {
        entries
            .get(1)
            .map(|entry| entry.as_slice())
            .ok_or_else(|| {
                AxVmError::invalid_config("guest Arm timer has no EL1 physical timer IRQ")
            })?
    };
    decode_gic_ppi(physical, "guest EL1 physical timer")
}

fn default_guest_physical_timer_ppi() -> AxVmResult<PpiId> {
    PpiId::new(DEFAULT_GUEST_PHYSICAL_TIMER_INTID as u8)
        .map_err(|error| AxVmError::interrupt("classify default physical timer", error))
}

fn decode_gic_ppi(specifier: &[u32], role: &'static str) -> AxVmResult<PpiId> {
    if specifier.len() != GIC_INTERRUPT_CELLS || specifier[0] != 1 {
        return Err(AxVmError::invalid_config(format!(
            "{role} must use a three-cell GIC PPI specifier"
        )));
    }
    let raw = specifier[1]
        .checked_add(16)
        .ok_or_else(|| AxVmError::invalid_config(format!("{role} INTID overflows u32")))?;
    let raw = u8::try_from(raw)
        .map_err(|_| AxVmError::invalid_config(format!("{role} INTID {raw} is not a PPI")))?;
    PpiId::new(raw)
        .map_err(|error| AxVmError::invalid_config(format!("{role} is invalid: {error}")))
}

fn parse_fdt(bytes: &[u8], owner: &'static str) -> AxVmResult<Fdt> {
    Fdt::from_bytes(bytes).map_err(|error| {
        AxVmError::invalid_config(format!(
            "failed to parse {owner} FDT for IRQ roles: {error:?}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn standard_roles_need_no_board_irq_configuration() {
        let roles = Aarch64InterruptRoles::from_discovered_intids(DiscoveredInterruptIds {
            host_ipi: 0,
            host_timer: 26,
            maintenance: 25,
            guest_timer: PpiId::new(30).unwrap(),
            passthrough: &[237],
        })
        .unwrap();

        assert!(roles.host_reserved().contains(&IntId::new(0).unwrap()));
        assert!(roles.host_reserved().contains(&IntId::new(25).unwrap()));
        assert!(roles.host_reserved().contains(&IntId::new(26).unwrap()));
        assert_eq!(roles.guest_physical_timer(), PpiId::new(30).unwrap());
    }

    #[test]
    fn internally_discovered_core_irq_conflicts_are_rejected() {
        let error = Aarch64InterruptRoles::from_discovered_intids(DiscoveredInterruptIds {
            host_ipi: 0,
            host_timer: 25,
            maintenance: 25,
            guest_timer: default_guest_physical_timer_ppi().unwrap(),
            passthrough: &[],
        })
        .unwrap_err();

        assert!(error.to_string().contains("reuses core INTID 25"));
    }

    #[test]
    fn passthrough_device_cannot_claim_an_internally_reserved_interrupt() {
        let error = Aarch64InterruptRoles::from_discovered_intids(DiscoveredInterruptIds {
            host_ipi: 0,
            host_timer: 26,
            maintenance: 25,
            guest_timer: default_guest_physical_timer_ppi().unwrap(),
            passthrough: &[26],
        })
        .unwrap_err();

        assert!(error.to_string().contains("passthrough device INTID 26"));
    }

    #[test]
    fn passthrough_device_cannot_claim_a_guest_timer_interrupt() {
        let error = Aarch64InterruptRoles::from_discovered_intids(DiscoveredInterruptIds {
            host_ipi: 0,
            host_timer: 26,
            maintenance: 25,
            guest_timer: default_guest_physical_timer_ppi().unwrap(),
            passthrough: &[30],
        })
        .unwrap_err();

        assert!(error.to_string().contains("guest timer"));
    }

    #[test]
    fn timer_binding_order_selects_nonsecure_physical_ppi() {
        let entries = [
            &[1, 13, 4][..],
            &[1, 14, 4][..],
            &[1, 11, 4][..],
            &[1, 10, 4][..],
        ];

        assert_eq!(
            select_guest_physical_timer_ppi(&entries, None).unwrap(),
            PpiId::new(30).unwrap()
        );
    }
}

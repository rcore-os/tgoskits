//! Discovery and validation of AArch64 architectural interrupt roles.

use alloc::{collections::BTreeSet, format, vec, vec::Vec};

use arm_vgic::{IntId, PpiId, PrivateInterruptMask};
use fdt_edit::{Fdt, Status};

use crate::{AxVmError, AxVmResult};

const DEFAULT_GIC_MAINTENANCE_INTID: u32 = 25;
const DEFAULT_GUEST_PHYSICAL_TIMER_INTID: u32 = 30;
const DEFAULT_GUEST_VIRTUAL_TIMER_INTID: u32 = 27;
const GIC_INTERRUPT_CELLS: usize = 3;

/// VM-internal classification of host-reserved and guest timer interrupts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Aarch64InterruptRoles {
    host_reserved: BTreeSet<IntId>,
    guest_timers: Vec<PpiId>,
    guest_private: PrivateInterruptMask,
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
    guest_timers: Vec<PpiId>,
    passthrough: &'a [u32],
}

impl Aarch64InterruptRoles {
    pub(crate) fn discover(discovery: Aarch64InterruptDiscovery<'_>) -> AxVmResult<Self> {
        let maintenance = match discovery.host_fdt_bytes {
            Some(bytes) => discover_maintenance_intid(bytes)?,
            None => DEFAULT_GIC_MAINTENANCE_INTID,
        };
        let guest_timers = match discovery.guest_fdt_bytes {
            Some(bytes) => discover_guest_timer_ppis(bytes)?,
            None => default_guest_timer_ppis()?,
        };
        Self::from_discovered_intids(DiscoveredInterruptIds {
            host_ipi: discovery.host_ipi_intid,
            host_timer: discovery.host_timer_intid,
            maintenance,
            guest_timers,
            passthrough: discovery.passthrough_intids,
        })
    }

    pub(crate) const fn guest_private_interrupts(&self) -> PrivateInterruptMask {
        self.guest_private
    }

    pub(crate) fn host_reserved(&self) -> &BTreeSet<IntId> {
        &self.host_reserved
    }

    pub(crate) fn guest_timers(&self) -> &[PpiId] {
        &self.guest_timers
    }

    fn from_discovered_intids(discovered: DiscoveredInterruptIds<'_>) -> AxVmResult<Self> {
        let DiscoveredInterruptIds {
            host_ipi,
            host_timer,
            maintenance,
            guest_timers,
            passthrough,
        } = discovered;
        let mut host_reserved = BTreeSet::new();
        for (role, raw) in [
            ("host IPI", host_ipi),
            ("host timer", host_timer),
            ("GIC maintenance", maintenance),
        ] {
            let intid = checked_core_intid(role, raw)?;
            if !host_reserved.insert(intid) {
                return Err(AxVmError::invalid_config(format!(
                    "internally discovered {role} reuses core INTID {raw}"
                )));
            }
        }

        for timer in &guest_timers {
            let timer = IntId::Ppi(*timer);
            if host_reserved.contains(&timer) {
                return Err(AxVmError::invalid_config(format!(
                    "guest timer INTID {} conflicts with an internally discovered host interrupt",
                    timer.raw()
                )));
            }
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
            if guest_timers
                .iter()
                .any(|timer| u32::from(timer.raw()) == raw)
            {
                return Err(AxVmError::invalid_config(format!(
                    "passthrough device INTID {raw} conflicts with a guest timer"
                )));
            }
        }

        let mut guest_private = PrivateInterruptMask::SGIS;
        for timer in &guest_timers {
            guest_private = guest_private
                .with(IntId::Ppi(*timer))
                .map_err(|error| AxVmError::interrupt("classify guest timer PPI", error))?;
        }
        Ok(Self {
            host_reserved,
            guest_timers,
            guest_private,
        })
    }
}

fn checked_core_intid(role: &'static str, raw: u32) -> AxVmResult<IntId> {
    IntId::new(raw).map_err(|error| {
        AxVmError::invalid_config(format!("{role} reports invalid GIC INTID {raw}: {error}"))
    })
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

fn discover_guest_timer_ppis(bytes: &[u8]) -> AxVmResult<Vec<PpiId>> {
    let fdt = parse_fdt(bytes, "guest")?;
    let Some(timer) = fdt
        .find_compatible(&["arm,armv8-timer"])
        .into_iter()
        .find(|node| node.as_node().status() != Some(Status::Disabled))
    else {
        return default_guest_timer_ppis();
    };
    let interrupts = timer
        .as_node()
        .get_property("interrupts")
        .ok_or_else(|| AxVmError::invalid_config("guest Arm timer has no interrupts property"))?;
    let cells = interrupts.get_u32_iter().collect::<Vec<_>>();
    if !cells.len().is_multiple_of(GIC_INTERRUPT_CELLS) {
        return Err(AxVmError::invalid_config(format!(
            "guest Arm timer interrupt property has {} cells, not complete GIC specifiers",
            cells.len()
        )));
    }
    let entries = cells.chunks_exact(GIC_INTERRUPT_CELLS).collect::<Vec<_>>();
    let names = timer
        .as_node()
        .get_property("interrupt-names")
        .map(|property| property.as_str_iter().collect::<Vec<_>>());
    select_guest_timer_ppis(&entries, names.as_deref())
}

fn select_guest_timer_ppis(entries: &[&[u32]], names: Option<&[&str]>) -> AxVmResult<Vec<PpiId>> {
    let (physical, virtual_timer) = if let Some(names) = names {
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
        let virtual_timer = names
            .iter()
            .position(|name| matches!(*name, "virt" | "virtual"))
            .ok_or_else(|| AxVmError::invalid_config("guest Arm timer has no virtual timer IRQ"))?;
        (entries[physical], entries[virtual_timer])
    } else {
        let physical = entries.get(1).copied().ok_or_else(|| {
            AxVmError::invalid_config("guest Arm timer has no EL1 physical timer IRQ")
        })?;
        let virtual_timer = entries
            .get(2)
            .copied()
            .ok_or_else(|| AxVmError::invalid_config("guest Arm timer has no virtual timer IRQ"))?;
        (physical, virtual_timer)
    };
    let physical = decode_gic_ppi(physical, "guest EL1 physical timer")?;
    let virtual_timer = decode_gic_ppi(virtual_timer, "guest virtual timer")?;
    if physical == virtual_timer {
        return Err(AxVmError::invalid_config(format!(
            "guest physical and virtual timers share PPI {}",
            physical.raw()
        )));
    }
    Ok(vec![physical, virtual_timer])
}

fn default_guest_timer_ppis() -> AxVmResult<Vec<PpiId>> {
    Ok(vec![
        PpiId::new(DEFAULT_GUEST_PHYSICAL_TIMER_INTID as u8)
            .map_err(|error| AxVmError::interrupt("classify default physical timer", error))?,
        PpiId::new(DEFAULT_GUEST_VIRTUAL_TIMER_INTID as u8)
            .map_err(|error| AxVmError::interrupt("classify default virtual timer", error))?,
    ])
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
            guest_timers: vec![PpiId::new(30).unwrap(), PpiId::new(27).unwrap()],
            passthrough: &[237],
        })
        .unwrap();

        assert!(roles.host_reserved().contains(&IntId::new(0).unwrap()));
        assert!(roles.host_reserved().contains(&IntId::new(25).unwrap()));
        assert!(roles.host_reserved().contains(&IntId::new(26).unwrap()));
        assert_eq!(
            roles.guest_timers(),
            &[PpiId::new(30).unwrap(), PpiId::new(27).unwrap()]
        );
        assert!(
            roles
                .guest_private_interrupts()
                .contains(IntId::new(27).unwrap())
        );
    }

    #[test]
    fn internally_discovered_core_irq_conflicts_are_rejected() {
        let error = Aarch64InterruptRoles::from_discovered_intids(DiscoveredInterruptIds {
            host_ipi: 0,
            host_timer: 25,
            maintenance: 25,
            guest_timers: default_guest_timer_ppis().unwrap(),
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
            guest_timers: default_guest_timer_ppis().unwrap(),
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
            guest_timers: default_guest_timer_ppis().unwrap(),
            passthrough: &[27],
        })
        .unwrap_err();

        assert!(error.to_string().contains("guest timer"));
    }

    #[test]
    fn timer_binding_order_selects_nonsecure_physical_and_virtual_ppis() {
        let entries = [
            &[1, 13, 4][..],
            &[1, 14, 4][..],
            &[1, 11, 4][..],
            &[1, 10, 4][..],
        ];

        assert_eq!(
            select_guest_timer_ppis(&entries, None).unwrap(),
            vec![PpiId::new(30).unwrap(), PpiId::new(27).unwrap()]
        );
    }
}

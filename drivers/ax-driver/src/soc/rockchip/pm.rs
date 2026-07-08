// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::{collections::BTreeMap, vec::Vec};

use log::{debug, info};
use rdif_power::{Interface as PowerInterface, PowerDomainId, PowerError};
use rdrive::{DriverGeneric, probe::OnProbeError, register::ProbeFdt};
use rockchip_pm::{PmError, PowerDomain, RkBoard, RockchipPM};

use crate::mmio::iomap;

crate::model_register!(
    name: "Rockchip Pm",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-power-controller"],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let parent = info.node.parent().ok_or_else(|| {
        OnProbeError::other(alloc::format!("[{}] has no PMU parent", info.node.name()))
    })?;
    let base_reg = parent
        .regs()
        .into_iter()
        .next()
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] PMU parent has no reg",
            parent.name()
        )))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000) as usize;
    let board = RkBoard::Rk3588;

    let mmio_base = iomap(base_reg.address as usize, mmio_size)?;
    let pm = RockchipPM::new(mmio_base, board);
    let pm = PowerDrv {
        pm,
        parents: domain_parent_map(info.node),
    };

    plat_dev.register(rdif_power::Power::new(pm));
    info!("Rockchip power-domain provider registered successfully");
    Ok(())
}

struct PowerDrv {
    pm: RockchipPM,
    parents: BTreeMap<PowerDomainId, PowerDomainId>,
}

impl DriverGeneric for PowerDrv {
    fn name(&self) -> &str {
        "Rockchip-Power"
    }
}

impl PowerInterface for PowerDrv {
    fn power_on(&mut self, id: PowerDomainId) -> Result<(), PowerError> {
        let mut path = Vec::new();
        self.collect_power_on_path(id, &mut path)?;
        for domain in path.into_iter().rev() {
            self.pm
                .power_domain_on(to_rockchip_domain(domain)?)
                .map_err(map_pm_error)?;
            debug!("Rockchip power domain {:#x} enabled", domain.raw());
        }
        Ok(())
    }

    fn power_off(&mut self, id: PowerDomainId) -> Result<(), PowerError> {
        self.pm
            .power_domain_off(to_rockchip_domain(id)?)
            .map_err(map_pm_error)
    }
}

impl PowerDrv {
    fn collect_power_on_path(
        &self,
        id: PowerDomainId,
        path: &mut Vec<PowerDomainId>,
    ) -> Result<(), PowerError> {
        if path.contains(&id) {
            return Err(PowerError::Controller);
        }
        path.push(id);
        if let Some(parent) = self.parents.get(&id).copied() {
            self.collect_power_on_path(parent, path)?;
        }
        Ok(())
    }
}

fn to_rockchip_domain(id: PowerDomainId) -> Result<PowerDomain, PowerError> {
    usize::try_from(id.raw())
        .map(PowerDomain)
        .map_err(|_| PowerError::InvalidId)
}

fn map_pm_error(error: PmError) -> PowerError {
    match error {
        PmError::DomainNotFound => PowerError::InvalidId,
        PmError::Timeout => PowerError::Busy,
        PmError::HardwareError => PowerError::Controller,
    }
}

fn domain_parent_map(
    root: rdrive::probe::fdt::NodeType<'_>,
) -> BTreeMap<PowerDomainId, PowerDomainId> {
    let mut children = |node| rdrive::probe::fdt::child_nodes(node);
    domain_parent_map_with(root, &mut children)
}

fn domain_parent_map_with<'a>(
    root: rdrive::probe::fdt::NodeType<'a>,
    children: &mut impl FnMut(rdrive::probe::fdt::NodeType<'a>) -> Vec<rdrive::probe::fdt::NodeType<'a>>,
) -> BTreeMap<PowerDomainId, PowerDomainId> {
    let mut parents = BTreeMap::new();
    collect_domain_parents(root, None, children, &mut parents);
    parents
}

fn collect_domain_parents<'a>(
    node: rdrive::probe::fdt::NodeType<'a>,
    parent: Option<PowerDomainId>,
    children: &mut impl FnMut(rdrive::probe::fdt::NodeType<'a>) -> Vec<rdrive::probe::fdt::NodeType<'a>>,
    parents: &mut BTreeMap<PowerDomainId, PowerDomainId>,
) {
    let current = domain_id(node);
    if let (Some(id), Some(parent)) = (current, parent) {
        parents.insert(id, parent);
    }
    let next_parent = current.or(parent);
    for child in children(node) {
        collect_domain_parents(child, next_parent, children, parents);
    }
}

fn domain_id(node: rdrive::probe::fdt::NodeType<'_>) -> Option<PowerDomainId> {
    node.as_node()
        .get_property("reg")
        .and_then(|prop| prop.get_u32())
        .map(PowerDomainId::from)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use fdt_edit::{Fdt, Node, Property};

    use super::*;

    #[test]
    fn domain_parent_map_reads_power_controller_tree() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let provider = fdt.add_node(root, Node::new("power-controller"));
        let npu = fdt.add_node(
            provider,
            node_with_props("power-domain@8", &[prop_u32s("reg", &[8])]),
        );
        let nputop = fdt.add_node(
            npu,
            node_with_props("power-domain@9", &[prop_u32s("reg", &[9])]),
        );
        fdt.add_node(
            nputop,
            node_with_props("power-domain@10", &[prop_u32s("reg", &[10])]),
        );
        fdt.add_node(
            nputop,
            node_with_props("power-domain@11", &[prop_u32s("reg", &[11])]),
        );

        let mut children = |node: rdrive::probe::fdt::NodeType<'_>| {
            node.as_node()
                .children()
                .iter()
                .filter_map(|child_id| {
                    let child_name = fdt.node(*child_id)?.name();
                    fdt.get_by_path(&alloc::format!("{}/{}", node.path(), child_name))
                })
                .collect()
        };
        let parents =
            domain_parent_map_with(fdt.get_by_path("/power-controller").unwrap(), &mut children);

        assert_eq!(
            parents.get(&PowerDomainId::new(9)),
            Some(&PowerDomainId::new(8))
        );
        assert_eq!(
            parents.get(&PowerDomainId::new(10)),
            Some(&PowerDomainId::new(9))
        );
        assert_eq!(
            parents.get(&PowerDomainId::new(11)),
            Some(&PowerDomainId::new(9))
        );
    }

    fn node_with_props(name: &str, props: &[Property]) -> Node {
        let mut node = Node::new(name);
        for prop in props {
            node.set_property(prop.clone());
        }
        node
    }

    fn prop_u32s(name: &str, values: &[u32]) -> Property {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(&value.to_be_bytes());
        }
        Property::new(name, data)
    }
}

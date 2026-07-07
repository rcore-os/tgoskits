use alloc::{
    collections::{BTreeMap, btree_map::Entry, btree_set::BTreeSet},
    string::{String, ToString},
    vec::Vec,
};
use core::ptr::NonNull;

use ax_kspin::SpinNoPreempt as Mutex;
use fdt_edit::Node;
pub use fdt_edit::{ClockRef, Fdt, InterruptRef, NodeId, NodeType, Phandle, RegInfo, Status};
use rdif_pinctrl::{PinctrlDevice, PinctrlError};
use spin::Once;

use super::ProbeError;
use crate::{
    Descriptor, Device, DeviceId, PlatformDevice,
    error::DriverError,
    probe::OnProbeError,
    register::{DriverRegister, ProbeKind},
};

static SYSTEM: Once<System> = Once::new();

pub fn init(fdt_addr: NonNull<u8>) -> Result<(), DriverError> {
    let sys = System::new(fdt_addr)?;
    SYSTEM.call_once(|| sys);
    Ok(())
}

pub fn check_addr(fdt_addr: NonNull<u8>) -> Result<(), DriverError> {
    unsafe { Fdt::from_ptr(fdt_addr.as_ptr()) }
        .map(|_| ())
        .map_err(|error| DriverError::Fdt(format!("{error:?}")))
}

pub fn probe_register(
    register: &DriverRegister,
) -> Result<Vec<Result<(), OnProbeError>>, ProbeError> {
    let sys = system();
    sys.probe_register(register)
}

pub(crate) fn try_probe_register(
    register: &DriverRegister,
) -> Option<Result<Vec<Result<(), OnProbeError>>, ProbeError>> {
    SYSTEM.get().map(|system| system.probe_register(register))
}

pub(crate) fn system() -> &'static System {
    SYSTEM.get().expect("rdrive not init")
}

pub(crate) fn try_system() -> Option<&'static System> {
    SYSTEM.get()
}

pub struct FdtInfo<'a> {
    pub node: NodeType<'a>,
    phandle_2_device_id: BTreeMap<Phandle, DeviceId>,
}

#[derive(Clone, Debug)]
pub struct ResourcePrepareConfig {
    apply_assigned_clocks: bool,
    enable_clocks: bool,
    deassert_resets: bool,
    enable_power_domains: bool,
    supply_names: Vec<String>,
    clock_rates: Vec<String>,
}

impl Default for ResourcePrepareConfig {
    fn default() -> Self {
        Self {
            apply_assigned_clocks: false,
            enable_clocks: true,
            deassert_resets: true,
            enable_power_domains: false,
            supply_names: Vec::from([String::from("vmmc-supply"), String::from("vqmmc-supply")]),
            clock_rates: Vec::new(),
        }
    }
}

impl ResourcePrepareConfig {
    pub fn without_assigned_clocks(mut self) -> Self {
        self.apply_assigned_clocks = false;
        self
    }

    pub fn with_assigned_clocks(mut self) -> Self {
        self.apply_assigned_clocks = true;
        self
    }

    pub fn without_clock_enable(mut self) -> Self {
        self.enable_clocks = false;
        self
    }

    pub fn without_reset_deassert(mut self) -> Self {
        self.deassert_resets = false;
        self
    }

    pub fn without_power_domains(mut self) -> Self {
        self.enable_power_domains = false;
        self
    }

    pub fn with_power_domains(mut self) -> Self {
        self.enable_power_domains = true;
        self
    }

    pub fn with_supply(mut self, name: impl Into<String>) -> Self {
        self.supply_names.push(name.into());
        self
    }

    pub fn with_named_clock_rate(mut self, name: impl Into<String>) -> Self {
        self.clock_rates.push(name.into());
        self
    }
}

#[derive(Clone, Debug, Default)]
pub struct ResourcePrepareReport {
    clock_rates: BTreeMap<String, u64>,
}

impl ResourcePrepareReport {
    pub fn clock_rate(&self, name: &str) -> Option<u64> {
        self.clock_rates.get(name).copied()
    }

    fn insert_clock_rate(&mut self, name: String, rate: u64) {
        self.clock_rates.insert(name, rate);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResetRef {
    pub name: Option<String>,
    pub phandle: Phandle,
    pub cells: u32,
    pub specifier: Vec<u32>,
}

impl ResetRef {
    pub fn select(&self) -> Option<u32> {
        (self.cells > 0)
            .then(|| self.specifier.first().copied())
            .flatten()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PowerDomainRef {
    pub name: Option<String>,
    pub phandle: Phandle,
    pub cells: u32,
    pub specifier: Vec<u32>,
}

impl PowerDomainRef {
    pub fn select(&self) -> Option<u32> {
        (self.cells > 0)
            .then(|| self.specifier.first().copied())
            .flatten()
    }
}

#[derive(Clone)]
pub struct ResetLine {
    node_name: String,
    name: Option<String>,
    device: Device<rdif_reset::Reset>,
    id: rdif_reset::ResetId,
}

impl ResetLine {
    fn from_refs(node_name: &str, refs: Vec<ResetRef>) -> Result<Vec<Self>, OnProbeError> {
        refs.into_iter()
            .map(|reset| Self::from_ref(node_name, &reset))
            .collect()
    }

    fn from_ref(node_name: &str, reset: &ResetRef) -> Result<Self, OnProbeError> {
        if reset.cells != 1 {
            return Err(OnProbeError::other(format!(
                "[{node_name}] reset {} uses {} cells, only one-cell reset selectors are supported",
                reset_label(reset),
                reset.cells
            )));
        }
        let selector = reset.select().ok_or_else(|| {
            OnProbeError::other(format!(
                "[{node_name}] reset {} has no selector",
                reset_label(reset)
            ))
        })?;
        let provider_id = system()
            .phandle_to_device_id(reset.phandle)
            .ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{node_name}] reset provider phandle {:?} is not populated",
                    reset.phandle
                ))
            })?;
        let device = crate::get::<rdif_reset::Reset>(provider_id).map_err(|err| {
            OnProbeError::other(format!(
                "[{node_name}] reset provider {:?} has no rdif-reset interface: {err}",
                reset.phandle
            ))
        })?;

        Ok(Self {
            node_name: node_name.to_string(),
            name: reset.name.clone(),
            device,
            id: rdif_reset::ResetId::from(selector),
        })
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn id(&self) -> rdif_reset::ResetId {
        self.id
    }

    pub fn assert(&self) -> Result<(), OnProbeError> {
        self.with_reset("assert", |reset, id| reset.assert(id))
    }

    pub fn deassert(&self) -> Result<(), OnProbeError> {
        self.with_reset("deassert", |reset, id| reset.deassert(id))
    }

    pub fn reset(&self) -> Result<(), OnProbeError> {
        self.with_reset("reset", |reset, id| reset.reset(id))
    }

    fn with_reset(
        &self,
        operation: &'static str,
        f: impl FnOnce(
            &mut rdif_reset::Reset,
            rdif_reset::ResetId,
        ) -> Result<(), rdif_reset::ResetError>,
    ) -> Result<(), OnProbeError> {
        let mut reset = self.device.lock().map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to lock reset {}: {err}",
                self.node_name,
                self.label()
            ))
        })?;
        f(&mut reset, self.id).map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to {operation} reset {}: {err}",
                self.node_name,
                self.label()
            ))
        })
    }

    fn label(&self) -> String {
        match self.name() {
            Some(name) => format!("{name}({:#x})", self.id.raw()),
            None => format!("{:#x}", self.id.raw()),
        }
    }
}

#[derive(Clone)]
pub struct PowerDomainLine {
    node_name: String,
    name: Option<String>,
    device: Device<rdif_power::Power>,
    id: rdif_power::PowerDomainId,
}

impl PowerDomainLine {
    fn from_refs(node_name: &str, refs: Vec<PowerDomainRef>) -> Result<Vec<Self>, OnProbeError> {
        refs.into_iter()
            .map(|domain| Self::from_ref(node_name, &domain))
            .collect()
    }

    fn from_ref(node_name: &str, domain: &PowerDomainRef) -> Result<Self, OnProbeError> {
        if domain.cells != 1 {
            return Err(OnProbeError::other(format!(
                "[{node_name}] power domain {} uses {} cells, only one-cell power-domain \
                 selectors are supported",
                power_domain_label(domain),
                domain.cells
            )));
        }
        let selector = domain.select().ok_or_else(|| {
            OnProbeError::other(format!(
                "[{node_name}] power domain {} has no selector",
                power_domain_label(domain)
            ))
        })?;
        let provider_id = system()
            .phandle_to_device_id(domain.phandle)
            .ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{node_name}] power-domain provider phandle {:?} is not populated",
                    domain.phandle
                ))
            })?;
        let device = crate::get::<rdif_power::Power>(provider_id).map_err(|err| {
            OnProbeError::other(format!(
                "[{node_name}] power-domain provider {:?} has no rdif-power interface: {err}",
                domain.phandle
            ))
        })?;

        Ok(Self {
            node_name: node_name.to_string(),
            name: domain.name.clone(),
            device,
            id: rdif_power::PowerDomainId::from(selector),
        })
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn id(&self) -> rdif_power::PowerDomainId {
        self.id
    }

    pub fn power_on(&self) -> Result<(), OnProbeError> {
        self.with_power("power on", |power, id| power.power_on(id))
    }

    pub fn power_off(&self) -> Result<(), OnProbeError> {
        self.with_power("power off", |power, id| power.power_off(id))
    }

    pub fn is_powered(&self) -> Result<bool, OnProbeError> {
        let power = self.device.lock().map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to lock power domain {}: {err}",
                self.node_name,
                self.label()
            ))
        })?;
        power.is_powered(self.id).map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to query power domain {}: {err}",
                self.node_name,
                self.label()
            ))
        })
    }

    fn with_power(
        &self,
        operation: &'static str,
        f: impl FnOnce(
            &mut rdif_power::Power,
            rdif_power::PowerDomainId,
        ) -> Result<(), rdif_power::PowerError>,
    ) -> Result<(), OnProbeError> {
        let mut power = self.device.lock().map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to lock power domain {}: {err}",
                self.node_name,
                self.label()
            ))
        })?;
        f(&mut power, self.id).map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to {operation} power domain {}: {err}",
                self.node_name,
                self.label()
            ))
        })
    }

    fn label(&self) -> String {
        match self.name() {
            Some(name) => format!("{name}({:#x})", self.id.raw()),
            None => format!("{:#x}", self.id.raw()),
        }
    }
}

#[derive(Clone)]
pub struct ClockLine {
    node_name: String,
    name: Option<String>,
    device: Device<rdif_clk::Clk>,
    id: rdif_clk::ClockId,
}

impl ClockLine {
    fn from_refs(node_name: &str, refs: Vec<ClockRef>) -> Result<Vec<Self>, OnProbeError> {
        refs.into_iter()
            .filter_map(|clock| match clock.cells {
                0 => None,
                _ => Some(Self::from_ref(node_name, &clock)),
            })
            .collect()
    }

    fn from_ref(node_name: &str, clock: &ClockRef) -> Result<Self, OnProbeError> {
        if clock.cells != 1 {
            return Err(OnProbeError::other(format!(
                "[{node_name}] clock {} uses {} cells, only one-cell clock selectors are supported",
                clock_label(clock),
                clock.cells
            )));
        }
        let selector = clock.select().ok_or_else(|| {
            OnProbeError::other(format!(
                "[{node_name}] clock {} has no selector",
                clock_label(clock)
            ))
        })?;
        let provider_id = system()
            .phandle_to_device_id(clock.phandle)
            .ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{node_name}] clock provider phandle {:?} is not populated",
                    clock.phandle
                ))
            })?;
        let device = crate::get::<rdif_clk::Clk>(provider_id).map_err(|err| {
            OnProbeError::other(format!(
                "[{node_name}] clock provider {:?} has no rdif-clk interface: {err}",
                clock.phandle
            ))
        })?;

        Ok(Self {
            node_name: node_name.to_string(),
            name: clock.name.clone(),
            device,
            id: rdif_clk::ClockId::from(selector as usize),
        })
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn id(&self) -> rdif_clk::ClockId {
        self.id
    }

    pub fn enable(&self) -> Result<(), OnProbeError> {
        self.with_clock("enable", |clock, id| clock.enable(id))
    }

    pub fn set_rate(&self, rate: u64) -> Result<(), OnProbeError> {
        self.with_clock("set rate", |clock, id| clock.set_rate(id, rate))
    }

    pub fn rate(&self) -> Result<u64, OnProbeError> {
        let clock = self.device.lock().map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to lock clock {}: {err}",
                self.node_name,
                self.label()
            ))
        })?;
        clock.get_rate(self.id).map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to read clock {}: {err:?}",
                self.node_name,
                self.label()
            ))
        })
    }

    fn with_clock(
        &self,
        operation: &'static str,
        f: impl FnOnce(&mut rdif_clk::Clk, rdif_clk::ClockId) -> Result<(), rdif_clk::KError>,
    ) -> Result<(), OnProbeError> {
        let mut clock = self.device.lock().map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to lock clock {}: {err}",
                self.node_name,
                self.label()
            ))
        })?;
        f(&mut clock, self.id).map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to {operation} clock {}: {err:?}",
                self.node_name,
                self.label()
            ))
        })
    }

    fn label(&self) -> String {
        match self.name() {
            Some(name) => format!("{name}({:#x})", self.id.raw()),
            None => format!("{:#x}", self.id.raw()),
        }
    }
}

fn reset_label(reset: &ResetRef) -> String {
    match reset.name.as_deref() {
        Some(name) => name.to_string(),
        None => format!("phandle {:?}", reset.phandle),
    }
}

fn power_domain_label(domain: &PowerDomainRef) -> String {
    match domain.name.as_deref() {
        Some(name) => name.to_string(),
        None => format!("phandle {:?}", domain.phandle),
    }
}

fn clock_label(clock: &ClockRef) -> String {
    match clock.name.as_deref() {
        Some(name) => name.to_string(),
        None => format!("phandle {:?}", clock.phandle),
    }
}

pub fn reset_refs(node: NodeType<'_>) -> Result<Vec<ResetRef>, OnProbeError> {
    let Some(prop) = node.as_node().get_property("resets") else {
        return Ok(Vec::new());
    };
    let reset_names = node
        .as_node()
        .get_property("reset-names")
        .map(|prop| {
            prop.as_str_iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut reader = prop.as_reader();
    let mut refs = Vec::new();
    let mut index = 0;
    while let Some(phandle_raw) = reader.read_u32() {
        let phandle = Phandle::from(phandle_raw);
        let provider = system().get_by_phandle(phandle).ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] reset provider phandle {phandle:?} not found",
                node.name()
            ))
        })?;
        let cells = provider
            .as_node()
            .get_property("#reset-cells")
            .and_then(|prop| prop.get_u32())
            .ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{}] reset provider {} has no #reset-cells",
                    node.name(),
                    provider.name()
                ))
            })?;

        let mut specifier = Vec::with_capacity(cells as usize);
        for _ in 0..cells {
            let value = reader.read_u32().ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{}] has truncated resets entry for phandle {phandle:?}",
                    node.name()
                ))
            })?;
            specifier.push(value);
        }

        refs.push(ResetRef {
            name: reset_names.get(index).cloned(),
            phandle,
            cells,
            specifier,
        });
        index += 1;
    }
    Ok(refs)
}

fn power_domain_refs_from_node(
    node: NodeType<'_>,
    mut provider_cells: impl FnMut(Phandle) -> Result<(String, u32), OnProbeError>,
) -> Result<Vec<PowerDomainRef>, OnProbeError> {
    let Some(prop) = node.as_node().get_property("power-domains") else {
        return Ok(Vec::new());
    };
    let domain_names = node
        .as_node()
        .get_property("power-domain-names")
        .map(|prop| {
            prop.as_str_iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut reader = prop.as_reader();
    let mut refs = Vec::new();
    let mut index = 0;
    while let Some(phandle_raw) = reader.read_u32() {
        let phandle = Phandle::from(phandle_raw);
        let (_provider_name, cells) = provider_cells(phandle)?;

        let mut specifier = Vec::with_capacity(cells as usize);
        for _ in 0..cells {
            let value = reader.read_u32().ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{}] has truncated power-domains entry for phandle {phandle:?}",
                    node.name()
                ))
            })?;
            specifier.push(value);
        }

        refs.push(PowerDomainRef {
            name: domain_names.get(index).cloned(),
            phandle,
            cells,
            specifier,
        });
        index += 1;
    }
    Ok(refs)
}

pub fn power_domain_refs(node: NodeType<'_>) -> Result<Vec<PowerDomainRef>, OnProbeError> {
    power_domain_refs_from_node(node, |phandle| {
        let provider = system().get_by_phandle(phandle).ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] power-domain provider phandle {phandle:?} not found",
                node.name()
            ))
        })?;
        let cells = provider
            .as_node()
            .get_property("#power-domain-cells")
            .and_then(|prop| prop.get_u32())
            .ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{}] power-domain provider {} has no #power-domain-cells",
                    node.name(),
                    provider.name()
                ))
            })?;

        Ok((provider.name().to_string(), cells))
    })
}

fn clock_refs_from_node(
    node: NodeType<'_>,
    property: &str,
    names_property: Option<&str>,
    mut provider_cells: impl FnMut(Phandle) -> Result<(String, u32), OnProbeError>,
) -> Result<Vec<ClockRef>, OnProbeError> {
    let Some(prop) = node.as_node().get_property(property) else {
        return Ok(Vec::new());
    };
    let clock_names = names_property
        .and_then(|name| node.as_node().get_property(name))
        .map(|prop| {
            prop.as_str_iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut reader = prop.as_reader();
    let mut refs = Vec::new();
    let mut index = 0;
    while let Some(phandle_raw) = reader.read_u32() {
        if phandle_raw == 0 {
            index += 1;
            continue;
        }

        let phandle = Phandle::from(phandle_raw);
        let (_provider_name, cells) = provider_cells(phandle)?;

        let mut specifier = Vec::with_capacity(cells as usize);
        for _ in 0..cells {
            let value = reader.read_u32().ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{}] has truncated {property} entry for phandle {phandle:?}",
                    node.name()
                ))
            })?;
            specifier.push(value);
        }

        refs.push(ClockRef {
            name: clock_names.get(index).cloned(),
            phandle,
            cells,
            specifier,
        });
        index += 1;
    }
    Ok(refs)
}

pub fn clock_refs(node: NodeType<'_>) -> Result<Vec<ClockRef>, OnProbeError> {
    clock_refs_from_node(node, "clocks", Some("clock-names"), |phandle| {
        let provider = system().get_by_phandle(phandle).ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] clock provider phandle {phandle:?} not found",
                node.name()
            ))
        })?;
        let cells = provider
            .as_node()
            .get_property("#clock-cells")
            .and_then(|prop| prop.get_u32())
            .ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{}] clock provider {} has no #clock-cells",
                    node.name(),
                    provider.name()
                ))
            })?;

        Ok((provider.name().to_string(), cells))
    })
}

pub fn reset_lines(node: NodeType<'_>) -> Result<Vec<ResetLine>, OnProbeError> {
    let refs = reset_refs(node)?;
    ResetLine::from_refs(node.name(), refs)
}

pub fn clock_lines(node: NodeType<'_>) -> Result<Vec<ClockLine>, OnProbeError> {
    let refs = clock_refs(node)?;
    ClockLine::from_refs(node.name(), refs)
}

pub fn power_domain_lines(node: NodeType<'_>) -> Result<Vec<PowerDomainLine>, OnProbeError> {
    let refs = power_domain_refs(node)?;
    PowerDomainLine::from_refs(node.name(), refs)
}

pub fn child_nodes(node: NodeType<'_>) -> Vec<NodeType<'static>> {
    let parent_path = node.path();
    node.as_node()
        .children()
        .iter()
        .filter_map(|child_id| {
            let child_name = system().fdt().node(*child_id)?.name();
            let child_path = if parent_path == "/" {
                format!("/{child_name}")
            } else {
                format!("{parent_path}/{child_name}")
            };
            system().fdt().get_by_path(&child_path)
        })
        .collect()
}

impl<'a> FdtInfo<'a> {
    pub fn get_by_phandle(&self, phandle: Phandle) -> Option<NodeType<'a>> {
        system().get_by_phandle(phandle)
    }

    pub fn find_compatible(&self, compatible: &[&str]) -> Vec<NodeType<'a>> {
        system().find_compatible(compatible)
    }

    pub fn phandle_to_device_id(&self, phandle: Phandle) -> Option<DeviceId> {
        self.phandle_2_device_id.get(&phandle).copied()
    }

    pub fn find_clk_by_name(&self, name: &str) -> Option<ClockRef> {
        self.node
            .clocks()
            .into_iter()
            .find(|clock| clock.name.as_deref() == Some(name))
    }

    pub fn prepare_resources(
        &self,
        config: ResourcePrepareConfig,
    ) -> Result<ResourcePrepareReport, OnProbeError> {
        if config.apply_assigned_clocks {
            apply_assigned_clocks_for_info(self)?;
        }
        apply_supply_regulators(self, &config.supply_names)?;
        if config.enable_power_domains {
            for domain in self.power_domain_lines()? {
                domain.power_on()?;
            }
        }
        if config.enable_clocks {
            for clock in self.clock_lines()? {
                clock.enable()?;
            }
        }
        if config.deassert_resets {
            for reset in self.reset_lines()? {
                reset.deassert()?;
            }
        }

        let mut report = ResourcePrepareReport::default();
        for name in config.clock_rates {
            let Some(clock) = self.find_clock_line_by_name(&name)? else {
                continue;
            };
            report.insert_clock_rate(name, clock.rate()?);
        }
        Ok(report)
    }

    pub fn clocks(&self) -> Result<Vec<ClockRef>, OnProbeError> {
        clock_refs(self.node)
    }

    pub fn clock_lines(&self) -> Result<Vec<ClockLine>, OnProbeError> {
        clock_lines(self.node)
    }

    pub fn clock_line(&self, clock: &ClockRef) -> Result<ClockLine, OnProbeError> {
        ClockLine::from_ref(self.node.name(), clock)
    }

    pub fn find_clock_line_by_name(&self, name: &str) -> Result<Option<ClockLine>, OnProbeError> {
        let Some(clock) = self
            .clocks()?
            .into_iter()
            .find(|clock| clock.name.as_deref() == Some(name))
        else {
            return Ok(None);
        };
        if clock.cells == 0 {
            return Ok(None);
        }
        self.clock_line(&clock).map(Some)
    }

    pub fn resets(&self) -> Result<Vec<ResetRef>, OnProbeError> {
        reset_refs(self.node)
    }

    pub fn find_reset_by_name(&self, name: &str) -> Result<Option<ResetRef>, OnProbeError> {
        Ok(self
            .resets()?
            .into_iter()
            .find(|reset| reset.name.as_deref() == Some(name)))
    }

    pub fn reset_lines(&self) -> Result<Vec<ResetLine>, OnProbeError> {
        reset_lines(self.node)
    }

    pub fn find_reset_line_by_name(&self, name: &str) -> Result<Option<ResetLine>, OnProbeError> {
        Ok(self
            .reset_lines()?
            .into_iter()
            .find(|reset| reset.name() == Some(name)))
    }

    pub fn power_domains(&self) -> Result<Vec<PowerDomainRef>, OnProbeError> {
        power_domain_refs(self.node)
    }

    pub fn find_power_domain_by_name(
        &self,
        name: &str,
    ) -> Result<Option<PowerDomainRef>, OnProbeError> {
        Ok(self
            .power_domains()?
            .into_iter()
            .find(|domain| domain.name.as_deref() == Some(name)))
    }

    pub fn power_domain_lines(&self) -> Result<Vec<PowerDomainLine>, OnProbeError> {
        power_domain_lines(self.node)
    }

    pub fn find_power_domain_line_by_name(
        &self,
        name: &str,
    ) -> Result<Option<PowerDomainLine>, OnProbeError> {
        Ok(self
            .power_domain_lines()?
            .into_iter()
            .find(|domain| domain.name() == Some(name)))
    }

    pub fn interrupts(&self) -> Vec<InterruptRef> {
        self.node.interrupts()
    }
}

pub fn apply_assigned_clocks(node: NodeType<'_>) -> Result<(), OnProbeError> {
    let info = FdtInfo {
        node,
        phandle_2_device_id: system().phandle_2_device_id.clone(),
    };
    apply_assigned_clocks_for_info(&info)
}

fn apply_assigned_clocks_for_info(info: &FdtInfo<'_>) -> Result<(), OnProbeError> {
    let node = info.node;
    if node.as_node().get_property("#clock-cells").is_some() {
        return Ok(());
    }

    let clocks = clock_refs_from_property(info, "assigned-clocks")?;
    let rates = node
        .as_node()
        .get_property("assigned-clock-rates")
        .map(|prop| prop.get_u32_iter().map(u64::from).collect::<Vec<_>>())
        .unwrap_or_default();
    let node_phandle = node.as_node().phandle();
    for (index, clock) in clocks.into_iter().enumerate() {
        let Some(rate) = rates.get(index).copied() else {
            continue;
        };
        if rate == 0 || clock.cells == 0 || Some(clock.phandle) == node_phandle {
            continue;
        }
        if clock.cells != 1 {
            return Err(OnProbeError::other(format!(
                "[{}] assigned clock {} uses {} cells, only one-cell clock selectors are supported",
                node.name(),
                clock_label(&clock),
                clock.cells
            )));
        }
        let Some(provider_id) = info.phandle_to_device_id(clock.phandle) else {
            return Err(OnProbeError::other(format!(
                "[{}] assigned clock provider phandle {:?} is not populated",
                node.name(),
                clock.phandle
            )));
        };
        match crate::get::<rdif_clk::Clk>(provider_id) {
            Ok(_) => {}
            Err(crate::GetDeviceError::TypeNotMatch | crate::GetDeviceError::NotFound) => {
                continue;
            }
            Err(err) => {
                return Err(OnProbeError::other(format!(
                    "[{}] assigned clock provider {:?} has no rdif-clk interface: {err}",
                    node.name(),
                    clock.phandle
                )));
            }
        }

        ClockLine::from_ref(node.name(), &clock)?.set_rate(rate)?;
    }
    Ok(())
}

fn clock_refs_from_property(
    info: &FdtInfo<'_>,
    property_name: &'static str,
) -> Result<Vec<ClockRef>, OnProbeError> {
    clock_refs_from_node(info.node, property_name, None, |phandle| {
        let provider = info.get_by_phandle(phandle).ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] {property_name} provider phandle {phandle:?} not found",
                info.node.name()
            ))
        })?;
        let cells = provider
            .as_node()
            .get_property("#clock-cells")
            .and_then(|prop| prop.get_u32())
            .ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{}] {property_name} provider {} has no #clock-cells",
                    info.node.name(),
                    provider.name()
                ))
            })?;

        Ok((provider.name().to_string(), cells))
    })
}

fn apply_supply_regulators(
    info: &FdtInfo<'_>,
    supply_names: &[String],
) -> Result<(), OnProbeError> {
    for name in supply_names {
        let Some(phandle) = supply_phandle(info.node.as_node(), name) else {
            continue;
        };
        let Some(node) = info.get_by_phandle(phandle) else {
            return Err(OnProbeError::other(format!(
                "[{}] supply {name} phandle {:?} not found",
                info.node.name(),
                phandle
            )));
        };
        if !fixed_regulator_has_control(node.as_node()) {
            continue;
        }
        apply_fixed_regulator(info, node.as_node(), name)?;
    }
    Ok(())
}

fn supply_phandle(node: &Node, name: &str) -> Option<Phandle> {
    node.get_property(name)
        .and_then(|prop| prop.get_u32())
        .map(Phandle::from)
}

fn fixed_regulator_has_control(node: &Node) -> bool {
    node.compatibles()
        .any(|compatible| compatible == "regulator-fixed")
        && (node.get_property("gpios").is_some()
            || node.get_property("gpio").is_some()
            || node.get_property("pinctrl-0").is_some())
}

fn apply_fixed_regulator(
    info: &FdtInfo<'_>,
    regulator: &Node,
    supply_name: &str,
) -> Result<(), OnProbeError> {
    let pinctrl = crate::get_one::<PinctrlDevice>().ok_or_else(|| {
        OnProbeError::other(format!(
            "[{}] PinctrlDevice not found for controlled supply {supply_name}",
            info.node.name()
        ))
    })?;
    let mut pinctrl = pinctrl
        .lock()
        .map_err(|err| OnProbeError::other(format!("failed to lock PinctrlDevice: {err}")))?;
    match pinctrl.apply_fdt_fixed_regulator(system().fdt(), regulator, "rdrive-resource") {
        Ok(()) | Err(PinctrlError::NotAvailable) => Ok(()),
        Err(err) => Err(OnProbeError::other(format!(
            "[{}] failed to enable supply {supply_name}: {err}",
            info.node.name()
        ))),
    }
}

fn apply_power_domains(node: NodeType<'_>) -> Result<(), OnProbeError> {
    for domain in power_domain_lines(node)? {
        domain.power_on()?;
    }
    Ok(())
}

fn apply_default_pinctrl(node: NodeType<'_>) -> Result<(), OnProbeError> {
    let Some(pinctrl) = crate::get_one::<PinctrlDevice>() else {
        return Ok(());
    };
    let mut pinctrl = pinctrl
        .lock()
        .map_err(|err| OnProbeError::other(format!("failed to lock PinctrlDevice: {err}")))?;
    pinctrl
        .apply_fdt_default_state(system().fdt(), node.as_node())
        .map_err(|err| {
            OnProbeError::other(format!(
                "failed to apply default pinctrl for [{}]: {err}",
                node.name()
            ))
        })
}

pub struct ProbeFdt<'a> {
    info: FdtInfo<'a>,
    platform: PlatformDevice,
}

impl<'a> ProbeFdt<'a> {
    pub(crate) fn new(info: FdtInfo<'a>, platform: PlatformDevice) -> Self {
        Self { info, platform }
    }

    pub const fn info(&self) -> &FdtInfo<'a> {
        &self.info
    }

    pub fn into_platform_device(self) -> PlatformDevice {
        self.platform
    }

    pub fn into_parts(self) -> (FdtInfo<'a>, PlatformDevice) {
        (self.info, self.platform)
    }
}

pub type FnOnProbe = for<'a> fn(ProbeFdt<'a>) -> Result<(), OnProbeError>;

pub struct System {
    fdt: Fdt,
    phandle_2_device_id: BTreeMap<Phandle, DeviceId>,
    populated_paths: Mutex<BTreeMap<String, DeviceId>>,
    populated_nodes: Mutex<BTreeSet<NodeId>>,
}

unsafe impl Send for System {}

impl System {
    pub fn fdt(&self) -> &Fdt {
        &self.fdt
    }

    pub fn phandle_to_device_id(&self, phandle: Phandle) -> Option<DeviceId> {
        self.phandle_2_device_id.get(&phandle).copied()
    }

    pub fn path_to_device_id(&self, path: &str) -> Option<DeviceId> {
        self.populated_paths.lock().get(path).copied()
    }

    pub fn note_device_path(&self, path: &str, device_id: DeviceId) -> bool {
        if self.fdt.get_by_path(path).is_none() {
            return false;
        }
        match self.populated_paths.lock().entry(String::from(path)) {
            Entry::Vacant(entry) => {
                entry.insert(device_id);
                true
            }
            Entry::Occupied(entry) => *entry.get() == device_id,
        }
    }

    pub fn get_by_phandle(&self, phandle: Phandle) -> Option<NodeType<'_>> {
        self.fdt.get_by_phandle(phandle)
    }

    pub fn find_compatible(&self, compatible: &[&str]) -> Vec<NodeType<'_>> {
        self.fdt.find_compatible(compatible)
    }

    pub fn new(fdt_addr: NonNull<u8>) -> Result<Self, DriverError> {
        let fdt = unsafe { Fdt::from_ptr(fdt_addr.as_ptr()) }
            .map_err(|error| DriverError::Fdt(format!("{error:?}")))?;
        let mut phandle_2_device_id = BTreeMap::new();
        for node in fdt.all_nodes() {
            if let Some(phandle) = node.as_node().phandle() {
                phandle_2_device_id.insert(phandle, DeviceId::new());
            }
        }
        Ok(Self {
            fdt,
            phandle_2_device_id,
            populated_paths: Mutex::new(BTreeMap::new()),
            populated_nodes: Mutex::new(BTreeSet::new()),
        })
    }

    fn new_device_id(&self, phandle: Option<Phandle>) -> DeviceId {
        if let Some(phandle) = phandle {
            self.phandle_2_device_id[&phandle]
        } else {
            DeviceId::new()
        }
    }

    fn get_fdt_match_nodes<'a>(&'a self, register: &DriverRegister) -> Vec<ProbeFdtInfo<'a>> {
        let mut out = Vec::new();
        let mut matched_nodes = BTreeSet::new();
        for node in self.fdt.all_nodes() {
            if matches!(node.as_node().status(), Some(Status::Disabled)) {
                continue;
            }

            let node_compatibles = node.as_node().compatibles().collect::<Vec<_>>();

            for probe in register.probe_kinds {
                let &ProbeKind::Fdt {
                    compatibles,
                    on_probe,
                } = probe
                else {
                    continue;
                };

                for compatible in &node_compatibles {
                    if compatibles.contains(compatible) && matched_nodes.insert(node.id()) {
                        out.push(ProbeFdtInfo {
                            name: register.name,
                            node,
                            on_probe,
                        });
                    }
                }
            }
        }
        out
    }

    fn probe_register(
        &self,
        register: &DriverRegister,
    ) -> Result<Vec<Result<(), OnProbeError>>, ProbeError> {
        let node_ls = self.get_fdt_match_nodes(register);
        let mut out = Vec::new();
        for node_info in node_ls {
            let node_id = node_info.node.id();
            if self.populated_nodes.lock().contains(&node_id) {
                continue;
            }
            let node = node_info.node;
            let node_phandle = node.as_node().phandle();
            let id = self.new_device_id(node_phandle);

            let irq_parent = node
                .interrupt_parent()
                .filter(|p| Some(*p) != node_phandle)
                .and_then(|p| self.phandle_2_device_id.get(&p).copied());

            let phandle_map = self.phandle_2_device_id.clone();

            debug!("Probe [{}]->[{}]", node.name(), node_info.name);
            let res = apply_assigned_clocks(node)
                .and_then(|()| apply_power_domains(node))
                .and_then(|()| apply_default_pinctrl(node))
                .and_then(|()| {
                    let descriptor = Descriptor {
                        name: node_info.name,
                        device_id: id,
                        irq_parent,
                    };

                    (node_info.on_probe)(ProbeFdt::new(
                        FdtInfo {
                            node,
                            phandle_2_device_id: phandle_map,
                        },
                        PlatformDevice::new(descriptor),
                    ))
                });

            if res.is_ok() {
                self.populated_paths.lock().insert(node.path(), id);
                self.populated_nodes.lock().insert(node_id);
            }

            out.push(res);
        }

        Ok(out)
    }
}

struct ProbeFdtInfo<'a> {
    name: &'static str,
    node: NodeType<'a>,
    on_probe: FnOnProbe,
}

#[cfg(test)]
mod tests {
    use alloc::{format, vec};

    use fdt_edit::{Node, Property};

    use super::*;

    #[test]
    fn power_domain_refs_parse_names_and_provider_cells() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.add_node(
            root,
            node_with_props(
                "power-controller",
                &[
                    prop_u32s("phandle", &[0x61]),
                    prop_u32s("#power-domain-cells", &[1]),
                ],
            ),
        );
        let consumer = fdt.add_node(
            root,
            node_with_props(
                "npu@fdab0000",
                &[
                    prop_u32s("power-domains", &[0x61, 9, 0x61, 10, 0x61, 11]),
                    prop_strs("power-domain-names", &["npu0", "npu1", "npu2"]),
                ],
            ),
        );

        let refs =
            power_domain_refs_from_node(fdt.get_by_path("/npu@fdab0000").unwrap(), |phandle| {
                fdt.get_by_phandle(phandle)
                    .and_then(|provider| {
                        provider
                            .as_node()
                            .get_property("#power-domain-cells")
                            .and_then(|prop| prop.get_u32())
                            .map(|cells| (provider.name().to_string(), cells))
                    })
                    .ok_or_else(|| OnProbeError::other(format!("missing provider {phandle:?}")))
            })
            .unwrap();

        assert_eq!(
            refs,
            vec![
                PowerDomainRef {
                    name: Some("npu0".into()),
                    phandle: Phandle::from(0x61),
                    cells: 1,
                    specifier: vec![9],
                },
                PowerDomainRef {
                    name: Some("npu1".into()),
                    phandle: Phandle::from(0x61),
                    cells: 1,
                    specifier: vec![10],
                },
                PowerDomainRef {
                    name: Some("npu2".into()),
                    phandle: Phandle::from(0x61),
                    cells: 1,
                    specifier: vec![11],
                },
            ]
        );
        assert_eq!(fdt.node(consumer).unwrap().name(), "npu@fdab0000");
    }

    #[test]
    fn power_domain_refs_reject_truncated_provider_specifier() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let consumer = fdt.add_node(
            root,
            node_with_props("jpeg@fdba0000", &[prop_u32s("power-domains", &[0x61])]),
        );

        let err = power_domain_refs_from_node(fdt.get_by_path("/jpeg@fdba0000").unwrap(), |_| {
            Ok(("power-controller".into(), 1))
        })
        .unwrap_err();

        assert!(format!("{err}").contains("truncated power-domains entry"));
        assert_eq!(fdt.node(consumer).unwrap().name(), "jpeg@fdba0000");
    }

    #[test]
    fn clock_refs_parse_names_and_provider_cells() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.add_node(
            root,
            node_with_props(
                "clock-controller",
                &[
                    prop_u32s("phandle", &[0x44]),
                    prop_u32s("#clock-cells", &[1]),
                ],
            ),
        );
        let consumer = fdt.add_node(
            root,
            node_with_props(
                "usb@fcd00000",
                &[
                    prop_u32s("clocks", &[0x44, 11, 0x44, 12]),
                    prop_strs("clock-names", &["bus", "ref"]),
                ],
            ),
        );

        let refs = clock_refs_from_node(
            fdt.get_by_path("/usb@fcd00000").unwrap(),
            "clocks",
            Some("clock-names"),
            |phandle| {
                fdt.get_by_phandle(phandle)
                    .and_then(|provider| {
                        provider
                            .as_node()
                            .get_property("#clock-cells")
                            .and_then(|prop| prop.get_u32())
                            .map(|cells| (provider.name().to_string(), cells))
                    })
                    .ok_or_else(|| OnProbeError::other(format!("missing provider {phandle:?}")))
            },
        )
        .unwrap();

        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name.as_deref(), Some("bus"));
        assert_eq!(refs[0].phandle, Phandle::from(0x44));
        assert_eq!(refs[0].cells, 1);
        assert_eq!(refs[0].specifier, vec![11]);
        assert_eq!(refs[1].name.as_deref(), Some("ref"));
        assert_eq!(refs[1].specifier, vec![12]);
        assert_eq!(fdt.node(consumer).unwrap().name(), "usb@fcd00000");
    }

    #[test]
    fn clock_refs_reject_truncated_provider_specifier() {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let consumer = fdt.add_node(
            root,
            node_with_props("pcie@fe150000", &[prop_u32s("clocks", &[0x44])]),
        );

        let err = clock_refs_from_node(
            fdt.get_by_path("/pcie@fe150000").unwrap(),
            "clocks",
            Some("clock-names"),
            |_| Ok(("clock-controller".into(), 1)),
        )
        .unwrap_err();

        assert!(format!("{err}").contains("truncated clocks entry"));
        assert_eq!(fdt.node(consumer).unwrap().name(), "pcie@fe150000");
    }

    #[test]
    fn clock_lines_reject_multi_cell_provider() {
        let clock = ClockRef {
            name: Some("core".into()),
            phandle: Phandle::from(0x44),
            cells: 2,
            specifier: vec![11, 0],
        };

        let err = match ClockLine::from_refs("device@2000", vec![clock]) {
            Ok(_) => panic!("multi-cell clock provider should be rejected"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("only one-cell clock selectors are supported"));
    }

    #[test]
    fn clock_lines_skip_zero_cell_provider() {
        let clock = ClockRef {
            name: Some("utmi".into()),
            phandle: Phandle::from(0x44),
            cells: 0,
            specifier: Vec::new(),
        };

        let lines = ClockLine::from_refs("usb@fc800000", vec![clock]).unwrap();

        assert!(lines.is_empty());
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

    fn prop_strs(name: &str, values: &[&str]) -> Property {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(value.as_bytes());
            data.push(0);
        }
        Property::new(name, data)
    }
}

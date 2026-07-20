use alloc::{string::String, vec::Vec};

use crate::{FunctionId, GpioLineId, GroupId, PinId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinDesc {
    pub id: PinId,
    pub name: Option<&'static str>,
}

impl PinDesc {
    pub const fn new(id: PinId, name: Option<&'static str>) -> Self {
        Self { id, name }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinGroup {
    pub id: GroupId,
    pub name: Option<&'static str>,
    pub pins: Vec<PinId>,
}

impl PinGroup {
    pub fn new(id: GroupId, name: Option<&'static str>, pins: Vec<PinId>) -> Self {
        Self { id, name, pins }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinFunction {
    pub id: FunctionId,
    pub name: Option<&'static str>,
    pub groups: Vec<GroupId>,
}

impl PinFunction {
    pub fn new(id: FunctionId, name: Option<&'static str>, groups: Vec<GroupId>) -> Self {
        Self { id, name, groups }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuxValue(u32);

impl MuxValue {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuxSetting {
    pub group: GroupId,
    pub function: FunctionId,
    pub value: MuxValue,
}

impl MuxSetting {
    pub const fn new(group: GroupId, function: FunctionId, value: MuxValue) -> Self {
        Self {
            group,
            function,
            value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bias {
    Disabled,
    BusHold,
    PullUp,
    PullDown,
    PullPinDefault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlewRate {
    Slow,
    Fast,
    Raw(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowPowerMode {
    Default,
    Sleep,
    Retention,
    HiZ,
    Raw(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinConfig {
    Bias(Bias),
    DriveStrengthUa(u32),
    SlewRate(SlewRate),
    InputEnable(bool),
    OutputEnable(bool),
    OutputValue(bool),
    DebounceUs(u32),
    LowPowerMode(LowPowerMode),
    Vendor { param: u32, value: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigTarget {
    Pin(PinId),
    Group(GroupId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigSetting {
    pub target: ConfigTarget,
    pub config: PinConfig,
}

impl ConfigSetting {
    pub const fn pin(pin: PinId, config: PinConfig) -> Self {
        Self {
            target: ConfigTarget::Pin(pin),
            config,
        }
    }

    pub const fn group(group: GroupId, config: PinConfig) -> Self {
        Self {
            target: ConfigTarget::Group(group),
            config,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateName {
    Default,
    Init,
    Idle,
    Sleep,
    Named(String),
    Index(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinState {
    name: StateName,
    muxes: Vec<MuxSetting>,
    configs: Vec<ConfigSetting>,
}

impl PinState {
    pub fn named(name: StateName) -> Self {
        Self {
            name,
            muxes: Vec::new(),
            configs: Vec::new(),
        }
    }

    pub fn with_mux(mut self, setting: MuxSetting) -> Self {
        self.muxes.push(setting);
        self
    }

    pub fn with_config(mut self, setting: ConfigSetting) -> Self {
        self.configs.push(setting);
        self
    }

    pub fn push_mux(&mut self, setting: MuxSetting) {
        self.muxes.push(setting);
    }

    pub fn push_config(&mut self, setting: ConfigSetting) {
        self.configs.push(setting);
    }

    pub fn extend(&mut self, other: PinState) {
        self.muxes.extend(other.muxes);
        self.configs.extend(other.configs);
    }

    pub fn name(&self) -> &StateName {
        &self.name
    }

    pub fn muxes(&self) -> &[MuxSetting] {
        &self.muxes
    }

    pub fn configs(&self) -> &[ConfigSetting] {
        &self.configs
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareKind {
    Static,
    Fdt,
    Acpi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpiPinStateSpec {
    path: String,
    state_name: StateName,
}

impl AcpiPinStateSpec {
    pub fn new(path: String, state_name: StateName) -> Self {
        Self { path, state_name }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn state_name(&self) -> &StateName {
        &self.state_name
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpiGpioLineSpec {
    path: String,
    line: GpioLineId,
    resource_index: u32,
}

impl AcpiGpioLineSpec {
    pub fn new(path: String, line: GpioLineId, resource_index: u32) -> Self {
        Self {
            path,
            line,
            resource_index,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn line(&self) -> GpioLineId {
        self.line
    }

    pub fn resource_index(&self) -> u32 {
        self.resource_index
    }
}

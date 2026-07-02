use alloc::{boxed::Box, vec::Vec};
use core::any::Any;

use rdif_base::DriverGeneric;

use crate::{
    ConfigSetting, ConfigTarget, FunctionId, GpioBank, GpioBankId, GpioIrqHandler, GpioIrqSourceId,
    GpioIrqSourceInfo, GpioRange, GroupId, PinDesc, PinFunction, PinGroup, PinState, PinctrlError,
};
#[cfg(feature = "fdt")]
use crate::{FdtPinctrl, FdtPinctrlParser};

#[cfg(feature = "fdt")]
pub type BFdtPinctrlParser = Box<dyn FdtPinctrlParser + Send>;

pub type BPinctrl = Box<dyn Interface>;
pub type BGpioBank = Box<dyn GpioBank>;
pub type BGpioIrqHandler = Box<dyn GpioIrqHandler>;

pub struct PinctrlDevice {
    interface: BPinctrl,
    #[cfg(feature = "fdt")]
    fdt_parser: Option<BFdtPinctrlParser>,
}

impl PinctrlDevice {
    pub fn new(interface: impl Interface + 'static) -> Self {
        Self {
            interface: Box::new(interface),
            #[cfg(feature = "fdt")]
            fdt_parser: None,
        }
    }

    #[cfg(feature = "fdt")]
    pub fn with_fdt_parser(
        interface: impl Interface + 'static,
        parser: impl FdtPinctrlParser + Send + 'static,
    ) -> Self {
        Self {
            interface: Box::new(interface),
            fdt_parser: Some(Box::new(parser)),
        }
    }

    pub fn boxed(interface: BPinctrl) -> Self {
        Self {
            interface,
            #[cfg(feature = "fdt")]
            fdt_parser: None,
        }
    }

    #[cfg(feature = "fdt")]
    pub fn boxed_with_fdt_parser(interface: BPinctrl, parser: BFdtPinctrlParser) -> Self {
        Self {
            interface,
            fdt_parser: Some(parser),
        }
    }

    pub fn interface(&self) -> &dyn Interface {
        self.interface.as_ref()
    }

    pub fn interface_mut(&mut self) -> &mut dyn Interface {
        self.interface.as_mut()
    }

    pub fn typed_ref<T: Interface>(&self) -> Option<&T> {
        self.raw_any()?.downcast_ref()
    }

    pub fn typed_mut<T: Interface>(&mut self) -> Option<&mut T> {
        self.raw_any_mut()?.downcast_mut()
    }

    #[cfg(feature = "fdt")]
    pub fn fdt_parser(&self) -> Option<&dyn FdtPinctrlParser> {
        self.fdt_parser
            .as_deref()
            .map(|parser| parser as &dyn FdtPinctrlParser)
    }

    #[cfg(feature = "fdt")]
    pub fn apply_fdt_default_state(
        &mut self,
        fdt: &fdt_edit::Fdt,
        node: &fdt_edit::Node,
    ) -> Result<(), PinctrlError> {
        if node.get_property("pinctrl-0").is_none() {
            return Ok(());
        }
        let Self {
            interface,
            fdt_parser,
        } = self;
        let Some(parser) = fdt_parser.as_deref() else {
            return Ok(());
        };
        FdtPinctrl::apply_state_from_consumer(interface.as_mut(), fdt, node, 0, parser)
    }

    #[cfg(feature = "fdt")]
    pub fn apply_fdt_fixed_regulator(
        &mut self,
        fdt: &fdt_edit::Fdt,
        regulator_node: &fdt_edit::Node,
        owner: &str,
    ) -> Result<(), PinctrlError> {
        let Self {
            interface,
            fdt_parser,
        } = self;
        let Some(parser) = fdt_parser.as_deref() else {
            return Ok(());
        };
        FdtPinctrl::apply_fixed_regulator(interface.as_mut(), fdt, regulator_node, parser, owner)
    }
}

impl DriverGeneric for PinctrlDevice {
    fn name(&self) -> &str {
        self.interface.name()
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self.interface.as_ref() as &dyn Any)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self.interface.as_mut() as &mut dyn Any)
    }
}

impl Interface for PinctrlDevice {
    fn pins(&self) -> &[PinDesc] {
        self.interface.pins()
    }

    fn groups(&self) -> &[PinGroup] {
        self.interface.groups()
    }

    fn functions(&self) -> &[PinFunction] {
        self.interface.functions()
    }

    fn gpio_ranges(&self) -> &[GpioRange] {
        self.interface.gpio_ranges()
    }

    fn can_mux(&self, group: GroupId, function: FunctionId) -> bool {
        self.interface.can_mux(group, function)
    }

    fn validate_state(&self, state: &PinState) -> Result<(), PinctrlError> {
        self.interface.validate_state(state)
    }

    fn apply_state(&mut self, state: &PinState) -> Result<(), PinctrlError> {
        self.interface.apply_state(state)
    }

    fn apply_mux(&mut self, setting: &crate::MuxSetting) -> Result<(), PinctrlError> {
        self.interface.apply_mux(setting)
    }

    fn apply_config(&mut self, setting: &ConfigSetting) -> Result<(), PinctrlError> {
        self.interface.apply_config(setting)
    }

    fn create_gpio_bank(&mut self, bank_id: GpioBankId) -> Option<BGpioBank> {
        self.interface.create_gpio_bank(bank_id)
    }

    fn irq_sources(&self) -> Vec<GpioIrqSourceInfo> {
        self.interface.irq_sources()
    }

    fn take_irq_handler(&mut self, source_id: GpioIrqSourceId) -> Option<BGpioIrqHandler> {
        self.interface.take_irq_handler(source_id)
    }
}

pub trait Interface: DriverGeneric {
    fn pins(&self) -> &[PinDesc] {
        &[]
    }

    fn groups(&self) -> &[PinGroup] {
        &[]
    }

    fn functions(&self) -> &[PinFunction] {
        &[]
    }

    fn gpio_ranges(&self) -> &[GpioRange] {
        &[]
    }

    fn can_mux(&self, group: GroupId, function: FunctionId) -> bool {
        self.functions()
            .iter()
            .find(|candidate| candidate.id == function)
            .is_some_and(|candidate| candidate.groups.contains(&group))
    }

    fn validate_state(&self, state: &PinState) -> Result<(), PinctrlError> {
        for mux in state.muxes() {
            if !self.groups().iter().any(|group| group.id == mux.group) {
                return Err(PinctrlError::InvalidGroup(mux.group));
            }
            if !self
                .functions()
                .iter()
                .any(|function| function.id == mux.function)
            {
                return Err(PinctrlError::InvalidFunction(mux.function));
            }
            if !self.can_mux(mux.group, mux.function) {
                return Err(PinctrlError::InvalidMux {
                    group: mux.group,
                    function: mux.function,
                });
            }
        }

        for config in state.configs() {
            match config.target {
                ConfigTarget::Pin(pin) => {
                    if !self.pins().iter().any(|desc| desc.id == pin) {
                        return Err(PinctrlError::InvalidPin(pin));
                    }
                }
                ConfigTarget::Group(group) => {
                    if !self.groups().iter().any(|desc| desc.id == group) {
                        return Err(PinctrlError::InvalidGroup(group));
                    }
                }
            }
        }

        Ok(())
    }

    fn apply_state(&mut self, state: &PinState) -> Result<(), PinctrlError> {
        self.validate_state(state)?;
        for mux in state.muxes() {
            self.apply_mux(mux)?;
        }
        for config in state.configs() {
            self.apply_config(config)?;
        }
        Ok(())
    }

    fn apply_mux(&mut self, _setting: &crate::MuxSetting) -> Result<(), PinctrlError> {
        Err(PinctrlError::NotSupported)
    }

    fn apply_config(&mut self, _setting: &ConfigSetting) -> Result<(), PinctrlError> {
        Err(PinctrlError::NotSupported)
    }

    fn create_gpio_bank(&mut self, _bank_id: GpioBankId) -> Option<BGpioBank> {
        None
    }

    fn irq_sources(&self) -> Vec<GpioIrqSourceInfo> {
        Vec::new()
    }

    fn take_irq_handler(&mut self, _source_id: GpioIrqSourceId) -> Option<BGpioIrqHandler> {
        None
    }
}

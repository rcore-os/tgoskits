extern crate alloc;

use rdif_intc::*;

use crate::fdt_parse_irq_config;

impl DriverGeneric for super::v2::Gic {
    fn name(&self) -> &str {
        "Arm GICv2 Driver"
    }
}

impl Interface for super::v2::Gic {
    fn translate_fdt(&self, irq_prop: &[u32]) -> Result<ControllerIrqTranslation, IrqError> {
        let config = fdt_parse_irq_config(irq_prop).map_err(|_| IrqError::InvalidIrq)?;
        Ok(ControllerIrqTranslation::with_trigger(
            HwIrq(config.id.to_u32()),
            config.trigger.into(),
        ))
    }

    fn configure(&mut self, translation: &IrqTranslation) -> Result<(), IrqError> {
        let config = crate::define::IrqConfig {
            id: unsafe { crate::define::IntId::raw(translation.id.hwirq.0) },
            trigger: translation.trigger.unwrap_or(Trigger::LevelHigh).into(),
        };
        self.set_cfg(config.id, config.trigger);
        Ok(())
    }

    fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
        self.set_irq_enable(unsafe { crate::define::IntId::raw(hwirq.0) }, enabled);
        Ok(())
    }
}

#[cfg(target_arch = "aarch64")]
impl DriverGeneric for super::v3::Gic {
    fn name(&self) -> &str {
        "Arm GICv3 Driver"
    }
}

#[cfg(target_arch = "aarch64")]
impl Interface for super::v3::Gic {
    fn translate_fdt(&self, irq_prop: &[u32]) -> Result<ControllerIrqTranslation, IrqError> {
        let config = fdt_parse_irq_config(irq_prop).map_err(|_| IrqError::InvalidIrq)?;
        Ok(ControllerIrqTranslation::with_trigger(
            HwIrq(config.id.to_u32()),
            config.trigger.into(),
        ))
    }

    fn configure(&mut self, translation: &IrqTranslation) -> Result<(), IrqError> {
        let config = crate::define::IrqConfig {
            id: unsafe { crate::define::IntId::raw(translation.id.hwirq.0) },
            trigger: translation.trigger.unwrap_or(Trigger::LevelHigh).into(),
        };
        self.set_cfg(config.id, config.trigger);
        Ok(())
    }

    fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
        self.set_irq_enable(unsafe { crate::define::IntId::raw(hwirq.0) }, enabled);
        Ok(())
    }
}

impl From<crate::define::Trigger> for Trigger {
    fn from(trigger: crate::define::Trigger) -> Self {
        match trigger {
            crate::define::Trigger::Edge => Trigger::EdgeRising,
            crate::define::Trigger::Level => Trigger::LevelHigh,
        }
    }
}

impl From<Trigger> for crate::define::Trigger {
    fn from(trigger: Trigger) -> Self {
        match trigger {
            Trigger::LevelLow => crate::define::Trigger::Level,
            Trigger::LevelHigh => crate::define::Trigger::Level,
            Trigger::EdgeRising => crate::define::Trigger::Edge,
            Trigger::EdgeBoth => crate::define::Trigger::Edge,
            Trigger::EdgeFailling => crate::define::Trigger::Edge,
        }
    }
}

impl From<crate::define::IrqConfig> for IrqConfig {
    fn from(config: crate::define::IrqConfig) -> Self {
        IrqConfig {
            irq: (config.id.to_u32() as usize).into(),
            trigger: match config.trigger {
                crate::v2::Trigger::Edge => Trigger::EdgeRising,
                crate::v2::Trigger::Level => Trigger::LevelHigh,
            },
            is_private: config.id.is_private(),
        }
    }
}

extern crate alloc;

use rdif_intc::*;

use crate::{checked_intid, fdt_parse_irq_config};

fn checked_rdif_intid(raw: u32, max_intid: u32) -> Result<crate::define::IntId, IrqError> {
    checked_intid(raw, max_intid).map_err(|_| IrqError::InvalidIrq)
}

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
            id: checked_rdif_intid(translation.id.hwirq.0, self.max_intid())?,
            trigger: translation.trigger.unwrap_or(Trigger::LevelHigh).into(),
        };
        self.set_cfg(config.id, config.trigger);
        Ok(())
    }

    fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
        let intid = checked_rdif_intid(hwirq.0, self.max_intid())?;
        self.set_irq_enable(intid, enabled);
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
            id: checked_rdif_intid(translation.id.hwirq.0, self.max_intid())?,
            trigger: translation.trigger.unwrap_or(Trigger::LevelHigh).into(),
        };
        self.set_cfg(config.id, config.trigger);
        Ok(())
    }

    fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
        let intid = checked_rdif_intid(hwirq.0, self.max_intid())?;
        self.set_irq_enable(intid, enabled);
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

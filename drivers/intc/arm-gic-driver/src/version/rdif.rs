extern crate alloc;

use rdif_intc::*;

use crate::fdt_parse_irq_config;

impl DriverGeneric for super::v2::Gic {
    fn name(&self) -> &str {
        "Arm GICv2 Driver"
    }
}

impl Interface for super::v2::Gic {
    fn setup_irq_by_fdt(&mut self, irq_prop: &[u32]) -> IrqId {
        let config = fdt_parse_irq_config(irq_prop).unwrap();
        self.set_cfg(config.id, config.trigger);
        config.id.into()
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
    fn setup_irq_by_fdt(&mut self, irq_prop: &[u32]) -> IrqId {
        let config = fdt_parse_irq_config(irq_prop).unwrap();
        self.set_cfg(config.id, config.trigger);
        config.id.into()
    }
}

impl From<crate::define::IntId> for IrqId {
    fn from(id: crate::define::IntId) -> Self {
        (id.to_u32() as usize).into()
    }
}

impl From<IrqId> for crate::define::IntId {
    fn from(id: IrqId) -> Self {
        let raw: usize = id.into();
        unsafe { crate::define::IntId::raw(raw as u32) }
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

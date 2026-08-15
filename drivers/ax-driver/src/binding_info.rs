use alloc::vec::Vec;

use axklib::irq::{legacy_irq_raw, try_legacy_irq};
use irq_framework::{AcpiGsiRoute, IrqId, IrqSource};
use rdrive::DeviceId;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BindingInfo {
    irqs: Vec<BindingIrqBinding>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingIrqBinding {
    pub source_id: usize,
    pub irq: BindingIrq,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingIrq {
    Id(IrqId),
    Source(BindingIrqSource),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingIrqSource {
    AcpiGsi(u32),
    AcpiGsiRoute(AcpiGsiRoute),
    FdtInterrupt(FdtIrqSpec),
}

/// Fully described FDT interrupt specifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FdtIrqSpec {
    pub controller: DeviceId,
    pub cells: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(feature = "pci")]
pub enum PciIrqRequirement {
    Optional,
    Required,
}

impl BindingInfo {
    pub const fn empty() -> Self {
        Self { irqs: Vec::new() }
    }

    pub fn with_irq(irq: Option<usize>) -> Result<Self, irq_framework::IrqError> {
        Ok(Self::with_binding_irq(
            irq.map(BindingIrq::try_legacy).transpose()?,
        ))
    }

    pub fn with_irq_id(irq: Option<IrqId>) -> Self {
        Self::with_binding_irq(irq.map(BindingIrq::id))
    }

    pub fn with_binding_irq(irq: Option<BindingIrq>) -> Self {
        match irq {
            Some(irq) => Self::with_irq_sources([(0, irq)]),
            None => Self::empty(),
        }
    }

    pub fn with_irq_sources(irqs: impl IntoIterator<Item = (usize, BindingIrq)>) -> Self {
        Self {
            irqs: irqs
                .into_iter()
                .map(|(source_id, irq)| BindingIrqBinding { source_id, irq })
                .collect(),
        }
    }

    pub fn irq(&self) -> Option<&BindingIrq> {
        self.irq_for_source(0)
            .or_else(|| self.irqs.first().map(|binding| &binding.irq))
    }

    pub fn irq_cloned(&self) -> Option<BindingIrq> {
        self.irq().cloned()
    }

    pub fn irq_for_source(&self, source_id: usize) -> Option<&BindingIrq> {
        self.irqs
            .iter()
            .find(|binding| binding.source_id == source_id)
            .map(|binding| &binding.irq)
    }

    pub fn irq_for_source_cloned(&self, source_id: usize) -> Option<BindingIrq> {
        self.irq_for_source(source_id).cloned()
    }

    pub fn irq_sources(&self) -> &[BindingIrqBinding] {
        &self.irqs
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.irq().and_then(BindingIrq::legacy_num)
    }

    pub fn irq_num_for_source(&self, source_id: usize) -> Option<usize> {
        self.irq_for_source(source_id)
            .and_then(BindingIrq::legacy_num)
    }
}

impl BindingIrq {
    pub const fn id(id: IrqId) -> Self {
        Self::Id(id)
    }

    pub fn try_legacy(raw: usize) -> Result<Self, irq_framework::IrqError> {
        Ok(Self::Id(try_legacy_irq(raw)?))
    }

    pub const fn acpi_gsi(gsi: u32) -> Self {
        Self::Source(BindingIrqSource::AcpiGsi(gsi))
    }

    pub const fn acpi_gsi_route(route: AcpiGsiRoute) -> Self {
        Self::Source(BindingIrqSource::AcpiGsiRoute(route))
    }

    pub fn fdt_interrupt_with_controller(controller: DeviceId, cells: impl Into<Vec<u32>>) -> Self {
        Self::Source(BindingIrqSource::fdt_interrupt_with_controller(
            controller, cells,
        ))
    }

    pub const fn irq_id(&self) -> Option<IrqId> {
        match self {
            Self::Id(id) => Some(*id),
            Self::Source(_) => None,
        }
    }

    pub fn legacy_num(&self) -> Option<usize> {
        self.irq_id().and_then(legacy_irq_raw)
    }

    pub fn as_irq_source(&self) -> Option<IrqSource> {
        match self {
            Self::Id(_) => None,
            Self::Source(source) => source.as_irq_source(),
        }
    }
}

impl BindingIrqSource {
    pub const fn acpi_gsi(gsi: u32) -> Self {
        Self::AcpiGsi(gsi)
    }

    pub const fn acpi_gsi_route(route: AcpiGsiRoute) -> Self {
        Self::AcpiGsiRoute(route)
    }

    pub fn fdt_interrupt_with_controller(controller: DeviceId, cells: impl Into<Vec<u32>>) -> Self {
        Self::FdtInterrupt(FdtIrqSpec {
            controller,
            cells: cells.into(),
        })
    }

    pub fn as_irq_source(&self) -> Option<IrqSource> {
        match self {
            Self::AcpiGsi(gsi) => Some(IrqSource::AcpiGsi(*gsi)),
            Self::AcpiGsiRoute(route) => Some(IrqSource::AcpiGsiRoute(*route)),
            Self::FdtInterrupt(_) => None,
        }
    }
}

impl From<rdif_intc::AcpiGsiRoute> for BindingIrq {
    fn from(route: rdif_intc::AcpiGsiRoute) -> Self {
        Self::Source(BindingIrqSource::from(route))
    }
}

impl From<rdif_intc::AcpiGsiRoute> for BindingIrqSource {
    fn from(route: rdif_intc::AcpiGsiRoute) -> Self {
        Self::AcpiGsiRoute(AcpiGsiRoute {
            gsi: route.gsi,
            vector: route.vector,
            controller: match route.controller {
                rdif_intc::AcpiGsiController::IoApic => irq_framework::AcpiGsiController::IoApic,
                rdif_intc::AcpiGsiController::PchPic => irq_framework::AcpiGsiController::PchPic,
            },
            controller_id: route.controller_id,
            controller_address: route.controller_address,
            controller_input: route.controller_input,
            trigger: match route.trigger {
                rdif_intc::AcpiIrqTrigger::Edge => irq_framework::AcpiIrqTrigger::Edge,
                rdif_intc::AcpiIrqTrigger::Level => irq_framework::AcpiIrqTrigger::Level,
            },
            polarity: match route.polarity {
                rdif_intc::AcpiIrqPolarity::ActiveHigh => {
                    irq_framework::AcpiIrqPolarity::ActiveHigh
                }
                rdif_intc::AcpiIrqPolarity::ActiveLow => irq_framework::AcpiIrqPolarity::ActiveLow,
            },
        })
    }
}

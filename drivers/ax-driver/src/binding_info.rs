#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BindingInfo {
    irq: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(feature = "pci")]
pub enum PciIrqRequirement {
    Optional,
    Required,
}

impl BindingInfo {
    pub const fn empty() -> Self {
        Self { irq: None }
    }

    pub const fn with_irq(irq: Option<usize>) -> Self {
        Self { irq }
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.irq
    }
}

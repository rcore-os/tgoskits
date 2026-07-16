#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::ops::{Deref, DerefMut};

pub use irq_framework::{HwIrq, IrqDomainId, IrqError, IrqId};
pub use rdif_base::{
    DriverGeneric, KError, io,
    irq::{AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, IrqConfig, Trigger},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqTranslation {
    pub id: IrqId,
    pub trigger: Option<Trigger>,
}

impl IrqTranslation {
    pub const fn new(id: IrqId) -> Self {
        Self { id, trigger: None }
    }

    pub const fn with_trigger(id: IrqId, trigger: Trigger) -> Self {
        Self {
            id,
            trigger: Some(trigger),
        }
    }

    pub const fn from_controller(
        domain: IrqDomainId,
        translation: ControllerIrqTranslation,
    ) -> Self {
        Self {
            id: IrqId::new(domain, translation.hwirq),
            trigger: translation.trigger,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControllerIrqTranslation {
    pub hwirq: HwIrq,
    pub trigger: Option<Trigger>,
}

impl ControllerIrqTranslation {
    pub const fn new(hwirq: HwIrq) -> Self {
        Self {
            hwirq,
            trigger: None,
        }
    }

    pub const fn with_trigger(hwirq: HwIrq, trigger: Trigger) -> Self {
        Self {
            hwirq,
            trigger: Some(trigger),
        }
    }
}

pub trait Interface: DriverGeneric {
    fn translate_fdt(&self, _irq_prop: &[u32]) -> Result<ControllerIrqTranslation, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn supports_acpi_gsi(&self, _route: &AcpiGsiRoute) -> bool {
        false
    }

    fn translate_acpi(&self, _route: &AcpiGsiRoute) -> Result<ControllerIrqTranslation, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn configure(&mut self, _translation: &IrqTranslation) -> Result<(), IrqError> {
        Ok(())
    }

    fn configure_acpi(
        &mut self,
        translation: &IrqTranslation,
        _route: &AcpiGsiRoute,
    ) -> Result<(), IrqError> {
        self.configure(translation)
    }

    fn set_enabled(&mut self, _hwirq: HwIrq, _enabled: bool) -> Result<(), IrqError> {
        Err(IrqError::Unsupported)
    }
}

pub struct Intc {
    domain: IrqDomainId,
    inner: Box<dyn Interface>,
}

impl Intc {
    pub fn new<T: Interface>(domain: IrqDomainId, driver: T) -> Self {
        Self {
            domain,
            inner: Box::new(driver),
        }
    }

    pub const fn domain(&self) -> IrqDomainId {
        self.domain
    }

    pub fn translate_fdt(&self, irq_prop: &[u32]) -> Result<IrqTranslation, IrqError> {
        self.inner
            .translate_fdt(irq_prop)
            .map(|translation| IrqTranslation::from_controller(self.domain, translation))
    }

    pub fn supports_acpi_gsi(&self, route: &AcpiGsiRoute) -> bool {
        self.inner.supports_acpi_gsi(route)
    }

    pub fn translate_acpi(&self, route: &AcpiGsiRoute) -> Result<IrqTranslation, IrqError> {
        self.inner
            .translate_acpi(route)
            .map(|translation| IrqTranslation::from_controller(self.domain, translation))
    }

    pub fn configure(&mut self, translation: &IrqTranslation) -> Result<(), IrqError> {
        if translation.id.domain != self.domain {
            return Err(IrqError::InvalidIrq);
        }
        self.inner.configure(translation)
    }

    pub fn configure_acpi(
        &mut self,
        translation: &IrqTranslation,
        route: &AcpiGsiRoute,
    ) -> Result<(), IrqError> {
        if translation.id.domain != self.domain {
            return Err(IrqError::InvalidIrq);
        }
        let expected = self.translate_acpi(route)?;
        if expected.id != translation.id {
            return Err(IrqError::InvalidIrq);
        }
        self.inner.configure_acpi(translation, route)
    }

    pub fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
        self.inner.set_enabled(hwirq, enabled)
    }

    pub fn typed_ref<T: Interface>(&self) -> Option<&T> {
        self.raw_any()?.downcast_ref()
    }

    pub fn typed_mut<T: Interface>(&mut self) -> Option<&mut T> {
        self.raw_any_mut()?.downcast_mut()
    }
}

impl DriverGeneric for Intc {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        Some(self.inner.as_ref() as &dyn core::any::Any)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self.inner.as_mut() as &mut dyn core::any::Any)
    }
}

impl Deref for Intc {
    type Target = dyn Interface;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref()
    }
}

impl DerefMut for Intc {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{
        sync::{Arc, Mutex},
        vec,
        vec::Vec,
    };

    use super::*;

    struct MockIntc {
        hwirq: HwIrq,
        enabled_calls: Arc<Mutex<Vec<(HwIrq, bool)>>>,
    }

    impl DriverGeneric for MockIntc {
        fn name(&self) -> &str {
            "mock-intc"
        }
    }

    impl Interface for MockIntc {
        fn translate_fdt(&self, _irq_prop: &[u32]) -> Result<ControllerIrqTranslation, IrqError> {
            Ok(ControllerIrqTranslation::new(self.hwirq))
        }

        fn translate_acpi(
            &self,
            _route: &AcpiGsiRoute,
        ) -> Result<ControllerIrqTranslation, IrqError> {
            Ok(ControllerIrqTranslation::new(self.hwirq))
        }

        fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
            self.enabled_calls.lock().unwrap().push((hwirq, enabled));
            Ok(())
        }
    }

    #[test]
    fn intc_translation_uses_its_own_domain() {
        let intc_a = Intc::new(
            IrqDomainId(11),
            MockIntc {
                hwirq: HwIrq(5),
                enabled_calls: Arc::new(Mutex::new(Vec::new())),
            },
        );
        let intc_b = Intc::new(
            IrqDomainId(12),
            MockIntc {
                hwirq: HwIrq(5),
                enabled_calls: Arc::new(Mutex::new(Vec::new())),
            },
        );

        assert_eq!(
            intc_a.translate_fdt(&[5]).unwrap().id,
            IrqId::new(IrqDomainId(11), HwIrq(5))
        );
        assert_eq!(
            intc_b.translate_fdt(&[5]).unwrap().id,
            IrqId::new(IrqDomainId(12), HwIrq(5))
        );
    }

    #[test]
    fn set_enabled_passes_controller_local_hwirq() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut intc = Intc::new(
            IrqDomainId(11),
            MockIntc {
                hwirq: HwIrq(5),
                enabled_calls: Arc::clone(&calls),
            },
        );

        intc.set_enabled(HwIrq(5), true).unwrap();

        assert_eq!(*calls.lock().unwrap(), vec![(HwIrq(5), true)]);
    }

    #[test]
    fn configure_acpi_rejects_route_translation_mismatch() {
        let mut intc = Intc::new(
            IrqDomainId(11),
            MockIntc {
                hwirq: HwIrq(5),
                enabled_calls: Arc::new(Mutex::new(Vec::new())),
            },
        );
        let route = AcpiGsiRoute {
            gsi: 5,
            controller: AcpiGsiController::IoApic,
            controller_id: 0,
            controller_address: 0xfec0_0000,
            controller_input: 5,
            trigger: AcpiIrqTrigger::Level,
            polarity: AcpiIrqPolarity::ActiveLow,
        };
        let mismatched = IrqTranslation::new(IrqId::new(IrqDomainId(11), HwIrq(6)));

        assert_eq!(
            intc.configure_acpi(&mismatched, &route),
            Err(IrqError::InvalidIrq)
        );
    }
}

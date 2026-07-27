use axtest::prelude::*;

use crate::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, ControllerIrqTranslation,
    DriverGeneric, HwIrq, Intc, Interface, IrqDomainId, IrqError, IrqId, IrqTranslation, Trigger,
};

struct MockIntc {
    hwirq: HwIrq,
    enabled_call: Option<(HwIrq, bool)>,
    configured: Option<IrqTranslation>,
}

impl MockIntc {
    const fn new(hwirq: HwIrq) -> Self {
        Self {
            hwirq,
            enabled_call: None,
            configured: None,
        }
    }
}

impl DriverGeneric for MockIntc {
    fn name(&self) -> &str {
        "mock-intc"
    }
}

impl Interface for MockIntc {
    fn translate_fdt(&self, irq_prop: &[u32]) -> Result<ControllerIrqTranslation, IrqError> {
        if irq_prop.is_empty() {
            return Err(IrqError::InvalidIrq);
        }
        Ok(ControllerIrqTranslation::with_trigger(
            self.hwirq,
            Trigger::LevelHigh,
        ))
    }

    fn supports_acpi_gsi(&self, route: &AcpiGsiRoute) -> bool {
        route.controller_input == self.hwirq.0 as u8
    }

    fn translate_acpi(&self, route: &AcpiGsiRoute) -> Result<ControllerIrqTranslation, IrqError> {
        if !self.supports_acpi_gsi(route) {
            return Err(IrqError::InvalidIrq);
        }
        Ok(ControllerIrqTranslation::new(self.hwirq))
    }

    fn configure(&mut self, translation: &IrqTranslation) -> Result<(), IrqError> {
        self.configured = Some(*translation);
        Ok(())
    }

    fn set_enabled(&mut self, hwirq: HwIrq, enabled: bool) -> Result<(), IrqError> {
        self.enabled_call = Some((hwirq, enabled));
        Ok(())
    }
}

fn route(input: u8) -> AcpiGsiRoute {
    AcpiGsiRoute {
        gsi: u32::from(input),
        vector: 37,
        controller: AcpiGsiController::IoApic,
        controller_id: 0,
        controller_address: 0xfec0_0000,
        controller_input: input,
        trigger: AcpiIrqTrigger::Level,
        polarity: AcpiIrqPolarity::ActiveLow,
    }
}

#[axtest]
fn rdif_intc_plain_translation_helpers_keep_ids_and_triggers() {
    let domain = IrqDomainId(7);
    let hwirq = HwIrq(5);
    let controller_translation = ControllerIrqTranslation::with_trigger(hwirq, Trigger::EdgeRising);
    let translation = IrqTranslation::from_controller(domain, controller_translation);
    ax_assert_eq!(translation.id, IrqId::new(domain, hwirq));
    ax_assert_eq!(translation.trigger, Some(Trigger::EdgeRising));
    ax_assert_eq!(
        IrqTranslation::with_trigger(IrqId::new(domain, hwirq), Trigger::LevelLow).trigger,
        Some(Trigger::LevelLow)
    );

    ax_assert_eq!(Trigger::EdgeFailling, Trigger::EdgeFailling);
}

#[axtest]
fn rdif_intc_default_interface_methods_report_unsupported_where_needed() {
    struct Minimal;

    impl DriverGeneric for Minimal {
        fn name(&self) -> &str {
            "minimal-intc"
        }
    }

    impl Interface for Minimal {}

    let mut minimal = Minimal;
    ax_assert_eq!(minimal.translate_fdt(&[1]), Err(IrqError::Unsupported));
    ax_assert!(!minimal.supports_acpi_gsi(&route(1)));
    ax_assert_eq!(
        minimal.translate_acpi(&route(1)),
        Err(IrqError::Unsupported)
    );
    ax_assert_eq!(
        minimal.set_enabled(HwIrq(1), true),
        Err(IrqError::Unsupported)
    );
    minimal
        .configure(&IrqTranslation::new(IrqId::new(IrqDomainId(1), HwIrq(1))))
        .unwrap();
}

#[axtest]
fn rdif_intc_wrapper_translates_domains_and_configures_matching_routes() {
    let mut intc = Intc::new(IrqDomainId(11), MockIntc::new(HwIrq(5)));
    ax_assert_eq!(intc.name(), "mock-intc");
    ax_assert_eq!(intc.domain(), IrqDomainId(11));

    let translation = intc.translate_fdt(&[5]).unwrap();
    ax_assert_eq!(translation.id, IrqId::new(IrqDomainId(11), HwIrq(5)));
    ax_assert_eq!(translation.trigger, Some(Trigger::LevelHigh));

    ax_assert_eq!(intc.translate_fdt(&[]), Err(IrqError::InvalidIrq));
    ax_assert!(intc.supports_acpi_gsi(&route(5)));
    ax_assert!(!intc.supports_acpi_gsi(&route(6)));
    ax_assert_eq!(
        intc.translate_acpi(&route(5)).unwrap().id,
        IrqId::new(IrqDomainId(11), HwIrq(5))
    );

    intc.configure(&translation).unwrap();
    ax_assert_eq!(
        intc.typed_ref::<MockIntc>().unwrap().configured,
        Some(translation)
    );
    intc.configure_acpi(&IrqTranslation::new(translation.id), &route(5))
        .unwrap();

    let wrong_domain = IrqTranslation::new(IrqId::new(IrqDomainId(12), HwIrq(5)));
    ax_assert_eq!(intc.configure(&wrong_domain), Err(IrqError::InvalidIrq));
    ax_assert_eq!(
        intc.configure_acpi(
            &IrqTranslation::new(IrqId::new(IrqDomainId(11), HwIrq(6))),
            &route(5)
        ),
        Err(IrqError::InvalidIrq)
    );

    intc.set_enabled(HwIrq(5), true).unwrap();
    ax_assert_eq!(
        intc.typed_ref::<MockIntc>().unwrap().enabled_call,
        Some((HwIrq(5), true))
    );
    intc.typed_mut::<MockIntc>().unwrap().enabled_call = None;
    ax_assert_eq!(intc.typed_ref::<MockIntc>().unwrap().enabled_call, None);
}

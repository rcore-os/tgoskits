use rdif_intc::{
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

#[test]
fn rdif_intc_wrapper_translates_domains_and_configures_matching_routes() {
    let mut intc = Intc::new(IrqDomainId(11), MockIntc::new(HwIrq(5)));

    let translation = intc.translate_fdt(&[5]).unwrap();
    assert_eq!(translation.id, IrqId::new(IrqDomainId(11), HwIrq(5)));
    assert_eq!(translation.trigger, Some(Trigger::LevelHigh));

    assert_eq!(intc.translate_fdt(&[]), Err(IrqError::InvalidIrq));
    assert!(intc.supports_acpi_gsi(&route(5)));
    assert!(!intc.supports_acpi_gsi(&route(6)));
    assert_eq!(
        intc.translate_acpi(&route(5)).unwrap().id,
        IrqId::new(IrqDomainId(11), HwIrq(5))
    );

    intc.configure(&translation).unwrap();
    assert_eq!(
        intc.typed_ref::<MockIntc>().unwrap().configured,
        Some(translation)
    );
    intc.configure_acpi(&IrqTranslation::new(translation.id), &route(5))
        .unwrap();

    let wrong_domain = IrqTranslation::new(IrqId::new(IrqDomainId(12), HwIrq(5)));
    assert_eq!(intc.configure(&wrong_domain), Err(IrqError::InvalidIrq));
    assert_eq!(
        intc.configure_acpi(
            &IrqTranslation::new(IrqId::new(IrqDomainId(11), HwIrq(6))),
            &route(5)
        ),
        Err(IrqError::InvalidIrq)
    );

    intc.set_enabled(HwIrq(5), true).unwrap();
    assert_eq!(
        intc.typed_ref::<MockIntc>().unwrap().enabled_call,
        Some((HwIrq(5), true))
    );
}

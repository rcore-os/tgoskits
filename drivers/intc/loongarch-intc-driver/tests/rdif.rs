#![cfg(feature = "rdif")]

mod common;

use common::{FakeIocsr, test_mmio};
use loongarch_intc_driver::{
    CpuIrqLine, EioIntcConfig, EioIntcParts, LioIntcConfig, LioIntcParts, PchPicConfig, PchPicParts,
};
use rdif_intc::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, HwIrq, Intc, Interface,
    IrqDomainId, IrqError, IrqId, IrqTranslation, Trigger,
};

#[test]
fn eio_rdif_returns_local_hwirq_and_rejects_empty_range_and_domain_mismatch() {
    let iocsr = FakeIocsr::new();
    iocsr.set_u64(0x420, 0x80);
    let parts = EioIntcParts::new(iocsr.clone(), EioIntcConfig::new(128).unwrap()).unwrap();
    assert_eq!(iocsr.read_u32(0x14a0), 0x0002_0001);
    assert!(!iocsr.writes().is_empty());
    let domain = IrqDomainId(11);
    let mut intc = Intc::new(domain, parts.controller);

    assert_eq!(
        intc.translate_fdt(&[5]).unwrap().id,
        IrqId::new(domain, HwIrq(5))
    );
    assert_eq!(intc.translate_fdt(&[]), Err(IrqError::InvalidIrq));
    assert_eq!(intc.translate_fdt(&[128]), Err(IrqError::InvalidIrq));
    assert_eq!(
        intc.configure(&IrqTranslation::new(IrqId::new(IrqDomainId(12), HwIrq(5)))),
        Err(IrqError::InvalidIrq)
    );
}

#[test]
fn pch_rdif_validates_acpi_identity_and_programs_local_configuration() {
    let mut backing = [0u64; 128];
    let mmio = test_mmio(0x1000_0000, &mut backing);
    mmio.write(0x20, u32::MAX);
    let parts = PchPicParts::new(mmio.clone(), PchPicConfig::new(64, 16, 7).unwrap()).unwrap();
    let domain = IrqDomainId(21);
    let mut intc = Intc::new(domain, parts.controller);

    assert_eq!(
        intc.translate_fdt(&[15]).unwrap().id,
        IrqId::new(domain, HwIrq(15))
    );
    assert_eq!(intc.translate_fdt(&[]), Err(IrqError::InvalidIrq));
    assert_eq!(intc.translate_fdt(&[16]), Err(IrqError::InvalidIrq));
    assert_eq!(
        intc.configure(&IrqTranslation::new(IrqId::new(IrqDomainId(22), HwIrq(5)))),
        Err(IrqError::InvalidIrq)
    );

    let route = pch_route(7, 0x1000_0000, 5);
    let translation = intc.translate_acpi(&route).unwrap();
    assert_eq!(translation.id, IrqId::new(domain, HwIrq(5)));
    intc.configure_acpi(&translation, &route).unwrap();
    assert_eq!(mmio.read::<u32>(0x60), 0);
    assert_eq!(mmio.read::<u32>(0x3e0), 1 << 5);
    assert_eq!(mmio.read::<u8>(0x200 + 5), 69);

    intc.set_enabled(HwIrq(5), true).unwrap();
    assert_eq!(mmio.read::<u32>(0x20), !(1 << 5));

    let wrong_identity = pch_route(8, 0x1000_0000, 5);
    assert!(!intc.supports_acpi_gsi(&wrong_identity));
    assert_eq!(
        intc.translate_acpi(&wrong_identity),
        Err(IrqError::Unsupported)
    );
    let mismatched = IrqTranslation::new(IrqId::new(domain, HwIrq(6)));
    assert_eq!(
        intc.configure_acpi(&mismatched, &route),
        Err(IrqError::InvalidIrq)
    );
}

#[test]
fn pch_rdif_rejects_unrepresentable_dual_edge_configuration() {
    let mut backing = [0u64; 128];
    let parts = PchPicParts::new(
        test_mmio(0x1000_0000, &mut backing),
        PchPicConfig::new(0, 16, 0).unwrap(),
    )
    .unwrap();
    let mut controller = parts.controller;
    let translation =
        IrqTranslation::with_trigger(IrqId::new(IrqDomainId(1), HwIrq(4)), Trigger::EdgeBoth);

    assert_eq!(
        Interface::configure(&mut controller, &translation),
        Err(IrqError::Unsupported)
    );
}

#[test]
fn lio_rdif_returns_local_hwirq_and_rejects_empty_range_and_domain_mismatch() {
    let mut registers = [0u64; 8];
    let mut isr = [0u32; 1];
    let config = LioIntcConfig::new(
        [Some(CpuIrqLine::new(2).unwrap()), None, None, None],
        [u32::MAX, 0, 0, 0],
    )
    .unwrap();
    let parts = LioIntcParts::new(
        test_mmio(0x1fe0_1400, &mut registers),
        test_mmio(0x1fe0_1040, &mut isr),
        config,
    )
    .unwrap();
    let domain = IrqDomainId(31);
    let mut intc = Intc::new(domain, parts.controller);

    assert_eq!(
        intc.translate_fdt(&[31]).unwrap().id,
        IrqId::new(domain, HwIrq(31))
    );
    assert_eq!(intc.translate_fdt(&[]), Err(IrqError::InvalidIrq));
    assert_eq!(intc.translate_fdt(&[32]), Err(IrqError::InvalidIrq));
    assert_eq!(
        intc.configure(&IrqTranslation::new(IrqId::new(IrqDomainId(32), HwIrq(5)))),
        Err(IrqError::InvalidIrq)
    );
    assert_eq!(
        intc.configure(&IrqTranslation::with_trigger(
            IrqId::new(domain, HwIrq(5)),
            Trigger::EdgeRising,
        )),
        Err(IrqError::Unsupported)
    );
}

fn pch_route(controller_id: u16, address: u64, input: u8) -> AcpiGsiRoute {
    AcpiGsiRoute {
        gsi: 64 + u32::from(input),
        vector: 0x30 + 64 + usize::from(input),
        controller: AcpiGsiController::PchPic,
        controller_id,
        controller_address: address,
        controller_input: input,
        trigger: AcpiIrqTrigger::Level,
        polarity: AcpiIrqPolarity::ActiveLow,
    }
}

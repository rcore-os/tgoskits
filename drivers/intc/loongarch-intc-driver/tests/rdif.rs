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

#[test]
fn lio_rdif_validates_acpi_identity_local_routes_and_level_high_configuration() {
    let mut registers = [0u64; 8];
    let mut isr_backing = [0u32; 1];
    let regs = test_mmio(0x2000_0000, &mut registers);
    let isr = test_mmio(0x2100_0000, &mut isr_backing);
    let config = LioIntcConfig::from_firmware_routes(
        [Some(CpuIrqLine::new(2).unwrap()), None, None, None],
        [(1 << 5) | (1 << 31), 0, 0, 0],
    )
    .unwrap();
    let parts = LioIntcParts::new(regs.clone(), isr, config).unwrap();
    let mut controller = parts.controller;
    // GSI/vector are deliberately independent of the controller-local input.
    let route = AcpiGsiRoute {
        gsi: 99,
        vector: 137,
        controller: AcpiGsiController::LioPic,
        controller_id: 0,
        controller_address: 0x2000_0000,
        controller_input: 5,
        trigger: AcpiIrqTrigger::Level,
        polarity: AcpiIrqPolarity::ActiveHigh,
    };
    let domain = IrqDomainId(31);
    let translation = IrqTranslation::new(IrqId::new(domain, HwIrq(5)));
    assert!(controller.supports_acpi_gsi(&route));
    assert_eq!(controller.translate_acpi(&route).unwrap().hwirq, HwIrq(5));
    Interface::configure_acpi(&mut controller, &translation, &route).unwrap();
    Interface::configure(
        &mut controller,
        &IrqTranslation::with_trigger(translation.id, Trigger::LevelHigh),
    )
    .unwrap();

    let boundary_route = AcpiGsiRoute {
        controller_input: 31,
        ..route
    };
    let boundary_translation = IrqTranslation::new(IrqId::new(domain, HwIrq(31)));
    assert_eq!(
        controller.translate_acpi(&boundary_route).unwrap().hwirq,
        HwIrq(31)
    );
    Interface::configure_acpi(&mut controller, &boundary_translation, &boundary_route).unwrap();
    Interface::set_enabled(&mut controller, HwIrq(31), true).unwrap();
    assert_eq!(regs.read::<u32>(0x28), 1 << 31);

    // Exercise the driver's own boundary; Intc also checks translation IDs.
    let mismatched = IrqTranslation::new(IrqId::new(domain, HwIrq(31)));
    assert_eq!(
        Interface::configure_acpi(&mut controller, &mismatched, &route),
        Err(IrqError::InvalidIrq)
    );
    for rejected in [
        AcpiGsiRoute {
            controller: AcpiGsiController::PchPic,
            ..route
        },
        AcpiGsiRoute {
            controller: AcpiGsiController::IoApic,
            ..route
        },
        AcpiGsiRoute {
            controller_address: route.controller_address + 0x1000,
            ..route
        },
        AcpiGsiRoute {
            controller_id: 1,
            ..route
        },
        AcpiGsiRoute {
            controller_input: 32,
            ..route
        },
        AcpiGsiRoute {
            controller_input: 6,
            ..route
        },
    ] {
        assert!(!controller.supports_acpi_gsi(&rejected));
        assert_eq!(
            controller.translate_acpi(&rejected),
            Err(IrqError::Unsupported)
        );
        assert_eq!(
            Interface::configure_acpi(&mut controller, &translation, &rejected),
            Err(IrqError::Unsupported)
        );
    }
    for unsupported in [
        AcpiGsiRoute {
            trigger: AcpiIrqTrigger::Edge,
            ..route
        },
        AcpiGsiRoute {
            polarity: AcpiIrqPolarity::ActiveLow,
            ..route
        },
    ] {
        assert_eq!(
            Interface::configure_acpi(&mut controller, &translation, &unsupported),
            Err(IrqError::Unsupported)
        );
    }
    let unsupported_translation = IrqTranslation::with_trigger(translation.id, Trigger::EdgeRising);
    assert_eq!(
        Interface::configure_acpi(&mut controller, &unsupported_translation, &route),
        Err(IrqError::Unsupported)
    );
    for input in [6, 32] {
        let invalid = IrqTranslation::new(IrqId::new(domain, HwIrq(input)));
        assert_eq!(
            Interface::configure(&mut controller, &invalid),
            Err(IrqError::InvalidIrq)
        );
        for enabled in [true, false] {
            assert_eq!(
                Interface::set_enabled(&mut controller, HwIrq(input), enabled),
                Err(IrqError::InvalidIrq)
            );
        }
    }
    // Rejected operations must not touch either W1 port or trigger registers.
    assert_eq!(regs.read::<u32>(0x28), 1 << 31);
    assert_eq!(regs.read::<u32>(0x2c), u32::MAX);
    assert_eq!(regs.read::<u32>(0x30), 0);
    assert_eq!(regs.read::<u32>(0x34), 0);
    Interface::set_enabled(&mut controller, HwIrq(31), false).unwrap();
    assert_eq!(regs.read::<u32>(0x2c), 1 << 31);
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

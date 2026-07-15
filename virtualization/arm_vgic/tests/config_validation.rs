use arm_vgic::{GicV3Config, GicV3HardwareCapabilities, GicV3MmioRegion, GicV3Mode, VgicError};

#[test]
fn rejects_unaligned_gic_register_frames() {
    let result = GicV3Config::new(
        GicV3Mode::Emulated,
        GicV3MmioRegion::new(0x0800_1000, 0x1_0000).unwrap(),
        GicV3MmioRegion::new(0x080a_0000, 0x2_0000).unwrap(),
        0x2_0000,
        1,
    );

    assert!(matches!(result, Err(VgicError::InvalidConfig { .. })));
}

#[test]
fn rejects_overlapping_distributor_redistributor_and_its_frames() {
    let overlapping_redistributor = GicV3Config::new(
        GicV3Mode::Emulated,
        GicV3MmioRegion::new(0x0800_0000, 0x2_0000).unwrap(),
        GicV3MmioRegion::new(0x0801_0000, 0x2_0000).unwrap(),
        0x2_0000,
        1,
    );
    assert!(matches!(
        overlapping_redistributor,
        Err(VgicError::InvalidConfig { .. })
    ));

    let overlapping_its =
        base_config().with_its(GicV3MmioRegion::new(0x080a_0000, 0x1_0000).unwrap());
    assert!(matches!(
        overlapping_its,
        Err(VgicError::InvalidConfig { .. })
    ));
}

#[test]
fn rejects_more_list_registers_than_gicv3_can_expose() {
    assert!(matches!(
        base_config().with_list_register_count(17),
        Err(VgicError::InvalidConfig { .. })
    ));
}

#[test]
fn physical_typer_with_512_intids_reports_480_spis() {
    let capabilities = GicV3HardwareCapabilities::from_distributor_typer(0x0f).unwrap();

    assert_eq!(capabilities.spi_count(), 480);
    assert_eq!(
        base_config()
            .with_spi_count(capabilities.spi_count())
            .unwrap()
            .spi_limit(),
        512
    );
}

fn base_config() -> GicV3Config {
    GicV3Config::new(
        GicV3Mode::Emulated,
        GicV3MmioRegion::new(0x0800_0000, 0x1_0000).unwrap(),
        GicV3MmioRegion::new(0x080a_0000, 0x2_0000).unwrap(),
        0x2_0000,
        1,
    )
    .unwrap()
}

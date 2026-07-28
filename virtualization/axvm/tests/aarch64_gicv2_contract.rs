const GIC_SOURCE: &str = include_str!("../src/arch/aarch64/gic.rs");

fn bool_constant(name: &str) -> bool {
    let prefix = format!("const {name}: bool = ");
    let declaration = GIC_SOURCE
        .lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("missing {name} declaration"));

    match declaration.trim_end_matches(';') {
        "false" => false,
        "true" => true,
        value => panic!("{name} must be a boolean literal, got {value}"),
    }
}

#[test]
fn gicv2_direct_injection_preserves_legacy_group_and_eoi_policy() {
    const GROUP1: &str = "GICV2_DIRECT_GROUP1";
    const EOI_MAINTENANCE: &str = "GICV2_DIRECT_EOI_MAINTENANCE";

    assert!(!bool_constant(GROUP1));
    assert!(bool_constant(EOI_MAINTENANCE));
    assert_eq!(
        GIC_SOURCE.matches(GROUP1).count(),
        2,
        "{GROUP1} must configure the GICv2 virtual interrupt"
    );
    assert_eq!(
        GIC_SOURCE.matches(EOI_MAINTENANCE).count(),
        2,
        "{EOI_MAINTENANCE} must configure the GICv2 virtual interrupt"
    );
}

const VIRTIO_DEV_FEATURES: &[&str] = &[
    "virtio-blk",
    "virtio-gpu",
    "virtio-input",
    "virtio-net",
    "virtio-socket",
];

const PCI_DYN_INTX_ROUTE_FEATURES: &[&str] = &[
    "intel-net",
    "ixgbe",
    "realtek-rtl8125",
    "virtio-net",
    "xhci-pci",
];

const PCI_DYN_ACPI_INTX_ROUTE_FEATURES: &[&str] =
    &["intel-net", "ixgbe", "realtek-rtl8125", "virtio-net"];

fn has_feature(feature: &str) -> bool {
    std::env::var(format!(
        "CARGO_FEATURE_{}",
        feature.to_uppercase().replace('-', "_")
    ))
    .is_ok()
}

fn has_any_feature(features: &[&str]) -> bool {
    features.iter().any(|feature| has_feature(feature))
}

fn enable_cfg_flag(key: &str) {
    println!("cargo:rustc-cfg={key}");
}

fn main() {
    let has_virtio_core = has_feature("virtio-core");
    let has_virtio_dev = has_any_feature(VIRTIO_DEV_FEATURES);
    let has_plat_static = has_feature("plat-static");
    let has_plat_dyn = has_feature("plat-dyn");
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_has_cvsd = matches!(target_arch.as_str(), "riscv32" | "riscv64");

    if has_plat_dyn {
        enable_cfg_flag("plat_dyn");
    } else if has_plat_static {
        enable_cfg_flag("plat_static");
    }
    if has_virtio_core || has_virtio_dev {
        enable_cfg_flag("virtio_dev");
    }
    if has_plat_dyn && has_any_feature(PCI_DYN_INTX_ROUTE_FEATURES) {
        enable_cfg_flag("pci_dyn_intx_route");
    }
    if has_plat_dyn && has_any_feature(PCI_DYN_ACPI_INTX_ROUTE_FEATURES) {
        enable_cfg_flag("pci_dyn_acpi_intx_route");
    }
    if has_any_feature(&["ahci", "bcm2835-sdhci"]) || (has_feature("cvsd") && target_has_cvsd) {
        enable_cfg_flag("sync_block_dev");
    }

    println!("cargo::rustc-check-cfg=cfg(plat_static)");
    println!("cargo::rustc-check-cfg=cfg(plat_dyn)");
    println!("cargo::rustc-check-cfg=cfg(virtio_dev)");
    println!("cargo::rustc-check-cfg=cfg(pci_dyn_intx_route)");
    println!("cargo::rustc-check-cfg=cfg(pci_dyn_acpi_intx_route)");
    println!("cargo::rustc-check-cfg=cfg(sync_block_dev)");
}

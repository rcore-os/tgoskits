const VIRTIO_DEV_FEATURES: &[&str] = &[
    "virtio-blk",
    "virtio-gpu",
    "virtio-input",
    "virtio-net",
    "virtio-socket",
];

fn make_cfg_values(str_list: &[&str]) -> String {
    str_list
        .iter()
        .map(|s| format!("{s:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

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

fn enable_cfg(key: &str, value: &str) {
    println!("cargo:rustc-cfg={key}=\"{value}\"");
}

fn enable_cfg_flag(key: &str) {
    println!("cargo:rustc-cfg={key}");
}

fn main() {
    let has_virtio_core = has_feature("virtio-core");
    let has_virtio_dev = has_any_feature(VIRTIO_DEV_FEATURES);
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_has_cvsd = matches!(target_arch.as_str(), "riscv32" | "riscv64");
    let has_pci = has_feature("pci");
    let has_fdt = has_feature("fdt");
    let has_static = has_any_feature(&[
        "pci",
        "virtio-blk",
        "virtio-gpu",
        "virtio-input",
        "virtio-net",
        "virtio-socket",
        "cvsd",
    ]);

    if has_pci {
        enable_cfg("probe", "pci");
    }
    if has_fdt {
        enable_cfg("probe", "fdt");
    }
    if has_static {
        enable_cfg("probe", "static");
    }
    if has_virtio_core || has_virtio_dev {
        enable_cfg_flag("virtio_dev");
    }
    if has_any_feature(&["ahci", "bcm2835-sdhci"]) || (has_feature("cvsd") && target_has_cvsd) {
        enable_cfg_flag("sync_block_dev");
    }

    println!(
        "cargo::rustc-check-cfg=cfg(probe, values({}))",
        make_cfg_values(&["pci", "fdt", "static"])
    );
    println!("cargo::rustc-check-cfg=cfg(virtio_dev)");
    println!("cargo::rustc-check-cfg=cfg(sync_block_dev)");
}

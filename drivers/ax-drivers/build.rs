const NET_DEV_FEATURES: &[&str] = &[
    "fxmac",
    "intel-net",
    "ixgbe",
    "realtek-rtl8125",
    "virtio-core",
    "virtio-net",
];
const BLOCK_DEV_FEATURES: &[&str] = &[
    "ahci",
    "bcm2835-sdhci",
    "cvsd",
    "ramdisk",
    "sdmmc",
    "virtio-blk",
];
const DISPLAY_DEV_FEATURES: &[&str] = &["virtio-gpu"];
const INPUT_DEV_FEATURES: &[&str] = &["virtio-input"];
const VSOCK_DEV_FEATURES: &[&str] = &["virtio-socket"];
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

    if has_pci {
        enable_cfg("probe", "pci");
    }
    if has_fdt {
        enable_cfg("probe", "fdt");
    }
    if has_virtio_core || has_virtio_dev {
        enable_cfg_flag("virtio_dev");
    }
    if has_any_feature(&["ahci", "bcm2835-sdhci", "sdmmc"])
        || (has_feature("cvsd") && target_has_cvsd)
    {
        enable_cfg_flag("sync_block_dev");
    }

    // Generate cfgs like `net_dev="virtio-net"`. Multiple devices may now be
    // selected in one category because registration is delegated to rdrive.
    for (dev_kind, feat_list) in [
        ("net", NET_DEV_FEATURES),
        ("block", BLOCK_DEV_FEATURES),
        ("display", DISPLAY_DEV_FEATURES),
        ("input", INPUT_DEV_FEATURES),
        ("vsock", VSOCK_DEV_FEATURES),
    ] {
        if !has_feature(dev_kind) {
            continue;
        }

        let mut selected = false;
        for feat in feat_list {
            if has_feature(feat) {
                enable_cfg(&format!("{dev_kind}_dev"), feat);
                selected = true;
            }
        }
        if !selected {
            enable_cfg(&format!("{dev_kind}_dev"), "dummy");
        }
    }

    println!(
        "cargo::rustc-check-cfg=cfg(probe, values({}))",
        make_cfg_values(&["pci", "fdt"])
    );
    println!("cargo::rustc-check-cfg=cfg(virtio_dev)");
    println!("cargo::rustc-check-cfg=cfg(sync_block_dev)");
    println!(
        "cargo::rustc-check-cfg=cfg(net_dev, values({}, \"dummy\"))",
        make_cfg_values(NET_DEV_FEATURES)
    );
    println!(
        "cargo::rustc-check-cfg=cfg(block_dev, values({}, \"dummy\"))",
        make_cfg_values(BLOCK_DEV_FEATURES)
    );
    println!(
        "cargo::rustc-check-cfg=cfg(display_dev, values({}, \"dummy\"))",
        make_cfg_values(DISPLAY_DEV_FEATURES)
    );
    println!(
        "cargo::rustc-check-cfg=cfg(input_dev, values({}, \"dummy\"))",
        make_cfg_values(INPUT_DEV_FEATURES)
    );
    println!(
        "cargo::rustc-check-cfg=cfg(vsock_dev, values({}, \"dummy\"))",
        make_cfg_values(VSOCK_DEV_FEATURES)
    );
}

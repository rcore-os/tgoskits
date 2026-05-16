const NET_DEV_FEATURES: &[&str] = &["fxmac", "ixgbe", "virtio-net"];
const BLOCK_DEV_FEATURES: &[&str] = &["ramdisk", "sdmmc", "cvsd", "bcm2835-sdhci", "virtio-blk"];
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
    let has_virtio_dev = has_any_feature(VIRTIO_DEV_FEATURES);
    if has_feature("bus-mmio") {
        enable_cfg("bus", "mmio");
    } else if has_feature("bus-pci") {
        enable_cfg("bus", "pci");
    } else if has_virtio_dev {
        enable_cfg("bus", "mmio");
    }
    if has_virtio_dev {
        enable_cfg_flag("virtio_dev");
    }

    // Generate cfgs like `net_dev="virtio-net"`. if `dyn` is not enabled, only one device is
    // selected for each device category. If no device is selected, `dummy` is selected.
    let is_dyn = has_feature("dyn");
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
                if !is_dyn {
                    break;
                }
            }
        }
        if !is_dyn && !selected {
            enable_cfg(&format!("{dev_kind}_dev"), "dummy");
        }
    }

    println!(
        "cargo::rustc-check-cfg=cfg(bus, values({}))",
        make_cfg_values(&["pci", "mmio"])
    );
    println!("cargo::rustc-check-cfg=cfg(virtio_dev)");
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

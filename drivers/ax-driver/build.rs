const VIRTIO_DEV_FEATURES: &[&str] = &["virtio-gpu", "virtio-input", "virtio-net", "virtio-socket"];

#[path = "build_support/wifi.rs"]
mod wifi;

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

fn optional_utf8_environment(name: &str) -> Option<String> {
    std::env::var_os(name).map(|value| {
        value
            .into_string()
            .unwrap_or_else(|_| panic!("{name} must contain UTF-8"))
    })
}

fn main() {
    println!("cargo:rerun-if-env-changed=STARRY_WIFI_SSID");
    println!("cargo:rerun-if-env-changed=STARRY_WIFI_PASSWORD");
    if has_feature("aic8800-wifi") {
        let ssid = optional_utf8_environment("STARRY_WIFI_SSID");
        let password = optional_utf8_environment("STARRY_WIFI_PASSWORD");
        wifi::validate(ssid.as_deref(), password.as_deref())
            .unwrap_or_else(|error| panic!("invalid compile-time Wi-Fi configuration: {error}"));
    }

    let has_virtio_core = has_feature("virtio-core");
    let has_virtio_dev = has_any_feature(VIRTIO_DEV_FEATURES);
    if has_virtio_core || has_virtio_dev {
        enable_cfg_flag("virtio_dev");
    }
    println!("cargo::rustc-check-cfg=cfg(virtio_dev)");
}

fn main() {
    println!("cargo:rerun-if-env-changed=AX_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=AX_GIC_V3");
    println!("cargo:rustc-check-cfg=cfg(gic_v3)");
    if let Ok(config_path) = std::env::var("AX_CONFIG_PATH") {
        println!("cargo:rerun-if-changed={config_path}");
    }
    if matches!(std::env::var("AX_GIC_V3").as_deref(), Ok("1"))
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64")
    {
        println!("cargo:rustc-cfg=gic_v3");
    }
}

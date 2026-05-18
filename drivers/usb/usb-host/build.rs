fn main() {
    println!("cargo::rustc-check-cfg=cfg(umod)");
    println!("cargo::rustc-check-cfg=cfg(kmod)");

    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os == "none" {
        println!("cargo::rustc-cfg=kmod");
    } else if std::env::var("CARGO_FEATURE_LIBUSB").is_ok() {
        println!("cargo::rustc-cfg=umod");
    }
}

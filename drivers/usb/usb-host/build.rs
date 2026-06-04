fn main() {
    println!("cargo::rustc-check-cfg=cfg(umod)");
    println!("cargo::rustc-check-cfg=cfg(kmod)");

    if std::env::var("CARGO_FEATURE_UMOD").is_ok() {
        println!("cargo::rustc-cfg=umod");
    } else {
        println!("cargo::rustc-cfg=kmod");
    }
}

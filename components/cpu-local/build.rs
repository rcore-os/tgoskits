fn main() {
    let kernel_tls = std::env::var_os("CARGO_FEATURE_TLS").is_some()
        && std::env::var_os("CARGO_FEATURE_USPACE").is_none();
    println!("cargo::rustc-check-cfg=cfg(kernel_tls)");
    if kernel_tls {
        println!("cargo::rustc-cfg=kernel_tls");
    }
}

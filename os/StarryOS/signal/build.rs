fn main() {
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    alias(
        "sa_restorer",
        [
            "x86_64",
            "x86",
            "powerpc",
            "powerpc64",
            "s390x",
            "arm",
            "aarch64",
        ]
        .contains(&target_arch.as_str()),
    );
}

/// Creates a cfg alias if `has_feature` is true.
/// `alias` must be a snake case string.
fn alias(alias: &str, has_feature: bool) {
    println!("cargo:rustc-check-cfg=cfg({alias})");
    if has_feature {
        println!("cargo:rustc-cfg={alias}");
    }
}

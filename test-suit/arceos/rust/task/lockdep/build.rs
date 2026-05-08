fn main() {
    println!("cargo:rerun-if-env-changed=FEATURES");
    println!("cargo:rustc-check-cfg=cfg(expected_lockdep)");

    let expects_lockdep = std::env::var_os("CARGO_FEATURE_LOCKDEP").is_some()
        || std::env::var("FEATURES")
            .ok()
            .map(|features| {
                features
                    .split(|ch: char| ch == ',' || ch.is_whitespace())
                    .any(|feature| feature == "lockdep")
            })
            .unwrap_or(false);

    if expects_lockdep {
        println!("cargo:rustc-cfg=expected_lockdep");
    }
}

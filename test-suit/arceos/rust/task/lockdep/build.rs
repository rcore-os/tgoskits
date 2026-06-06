fn main() {
    println!("cargo:rerun-if-env-changed=FEATURES");
    println!("cargo:rerun-if-env-changed=LOCKDEP_CASE");
    println!("cargo:rustc-check-cfg=cfg(expected_lockdep)");

    let lockdep_enabled = std::env::var_os("CARGO_FEATURE_LOCKDEP").is_some()
        || std::env::var("FEATURES")
            .ok()
            .map(|features| {
                features
                    .split(|ch: char| ch == ',' || ch.is_whitespace())
                    .any(|feature| feature == "lockdep")
            })
            .unwrap_or(false);
    let case = std::env::var("LOCKDEP_CASE").unwrap_or_default();
    let expects_lockdep = lockdep_enabled && case != "vfs-cache-single";

    if expects_lockdep {
        println!("cargo:rustc-cfg=expected_lockdep");
    }
}

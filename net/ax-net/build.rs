fn main() {
    // `axtest` is set workspace-wide via RUSTFLAGS when the StarryOS kernel
    // axtest target is built (see scripts/axbuild ktest). Declaring it here keeps
    // the `#[cfg(axtest)]` test hooks in this crate from tripping the
    // `unexpected_cfgs` lint during a normal (non-axtest) build or clippy run.
    println!("cargo:rustc-check-cfg=cfg(axtest)");
}

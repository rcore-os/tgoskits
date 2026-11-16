fn main() {
    // This crate keeps its host tests in the library target rather than a
    // standalone integration-test target, so cargo:rustc-link-arg-tests is
    // not available here.  cargo:rustc-link-arg covers the library test
    // binary produced by `cargo test -p ax-net`.
    let ld = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("host-test.ld");
    println!("cargo:rerun-if-changed={}", ld.display());
    println!("cargo:rustc-link-arg=-T{}", ld.display());
}

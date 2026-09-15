fn main() {
    let ld = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("host-test.ld");
    println!("cargo:rerun-if-changed={}", ld.display());
    println!("cargo:rustc-link-arg-tests=-T{}", ld.display());
}

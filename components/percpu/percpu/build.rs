use std::path::Path;

fn main() {
    if cfg!(feature = "host-test") {
        let ld_script_path = Path::new(std::env!("CARGO_MANIFEST_DIR")).join("host-test.ld");
        println!("cargo:rerun-if-changed={}", ld_script_path.display());
        println!("cargo:rustc-link-arg-tests=-T{}", ld_script_path.display());
    }
}

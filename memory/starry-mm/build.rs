use std::path::Path;

fn main() {
    if cfg!(target_os = "linux") {
        let linker = Path::new(std::env!("CARGO_MANIFEST_DIR"))
            .join("../../components/percpu/percpu/host-test.ld");
        println!("cargo:rerun-if-changed={}", linker.display());
        // The accounting tests are part of the library test target.
        println!("cargo:rustc-link-arg=-T{}", linker.display());
    }
}

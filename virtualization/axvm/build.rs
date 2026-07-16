use std::path::Path;

fn main() {
    if cfg!(target_os = "linux") {
        let linker_script = Path::new(std::env!("CARGO_MANIFEST_DIR")).join("percpu-test.x");
        println!("cargo::rerun-if-changed={}", linker_script.display());
        // `rustc-link-arg-tests` applies to explicit integration-test targets,
        // but Cargo builds `src/lib.rs` unit tests as the library target with
        // `--test`. The package-wide host-only arguments cover both forms.
        println!("cargo::rustc-link-arg=-no-pie");
        println!("cargo::rustc-link-arg=-T{}", linker_script.display());
    }
}

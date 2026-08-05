use std::path::Path;

#[test]
fn register_backends_are_split_by_architecture() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let register_dir = crate_dir.join("src/register");

    assert!(
        !crate_dir.join("src/register.rs").exists(),
        "the architecture register boundary must use register/mod.rs"
    );
    for backend in [
        "mod.rs",
        "host.rs",
        "x86_64.rs",
        "aarch64.rs",
        "riscv.rs",
        "loongarch64.rs",
    ] {
        assert!(
            register_dir.join(backend).is_file(),
            "missing architecture register backend {backend}"
        );
    }
    assert!(
        !register_dir.join("arm.rs").exists(),
        "an unsupported ARM32 backend must not silently return zero or no-op"
    );
}

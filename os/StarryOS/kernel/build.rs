fn main() {
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");
    println!("cargo:rustc-check-cfg=cfg(axtest)");

    if std::env::var_os("CARGO_CFG_TARGET_OS").is_some_and(|target_os| target_os == "linux") {
        // Host tests use the native linker rather than Starry's linker script,
        // which keeps the scope-local registry section for kernel images.
        println!("cargo::rustc-link-arg=-Wl,-z,nostart-stop-gc");
    }

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH must be set by Cargo");
    std::fs::write(
        std::path::Path::new(&out_dir).join("build_info.rs"),
        format!("pub const ARCH: &str = {arch:?};"),
    )
    .unwrap();
    let linker = format!("{out_dir}/linker.x");

    std::fs::write(&linker, include_str!("linker.ld")).unwrap();
    println!("cargo:rustc-link-search={out_dir}");

    let target_dir = std::path::Path::new(&out_dir).join("../../..");
    std::fs::write(target_dir.join("linker.x"), include_str!("linker.ld")).unwrap();
}

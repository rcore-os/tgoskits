fn main() {
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");
    println!("cargo:rustc-check-cfg=cfg(axtest)");

    if std::env::var_os("CARGO_CFG_TARGET_OS").is_some_and(|target_os| target_os == "linux") {
        // Host tests use the native linker rather than Starry's linker script,
        // which keeps the scope-local registry section for kernel images.
        println!("cargo::rustc-link-arg=-Wl,-z,nostart-stop-gc");
        // The scripts that delimit the recovery tables, and the boot template
        // that publishes the TSS offset, are linker input for a kernel image
        // and are absent here. Name the symbols they would have defined so the
        // code that reads them still links: the host registers no recovery
        // entry, and the trap entry that reads the offset never runs.
        for symbol in [
            "_ex_table_start",
            "_ex_table_end",
            "_nofault_ex_table_start",
            "_nofault_ex_table_end",
            "__CPU_LOCAL_TSS_OFFSET",
        ] {
            println!("cargo::rustc-link-arg=-Wl,--defsym={symbol}=0");
        }
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

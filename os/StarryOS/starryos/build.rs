fn main() {
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=../../../platforms/axplat-dyn/link.ld");
    println!("cargo:rerun-if-changed=../../../platforms/somehal/link.ld");
    println!("cargo:rerun-if-changed=../../../components/someboot/src/arch/aarch64/link.ld");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let linker = format!("{out_dir}/linker.x");

    std::fs::write(&linker, include_str!("linker.ld")).unwrap();
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        let axplat = include_str!("../../../platforms/axplat-dyn/link.ld").replace("{{SMP}}", "16");
        std::fs::write(format!("{out_dir}/axplat.x"), axplat).unwrap();
        std::fs::write(
            format!("{out_dir}/link.x"),
            include_str!("../../../platforms/somehal/link.ld"),
        )
        .unwrap();
        let someboot = include_str!("../../../components/someboot/src/arch/aarch64/link.ld")
            .replace("${kernel_load_vaddr}", "0xffffffff80000000");
        std::fs::write(format!("{out_dir}/someboot.x"), someboot).unwrap();
    }
    println!("cargo:rustc-link-search={out_dir}");

    let target_dir = std::path::Path::new(&out_dir).join("../../..");
    std::fs::write(target_dir.join("linker.x"), include_str!("linker.ld")).unwrap();
}

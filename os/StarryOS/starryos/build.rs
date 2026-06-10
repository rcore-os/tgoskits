fn main() {
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=../../../platforms/axplat-dyn/link.ld");
    println!("cargo:rerun-if-changed=../../../platforms/somehal/link.ld");
    println!("cargo:rerun-if-changed=../../../components/someboot/src/arch/aarch64/link.ld");
    println!("cargo:rerun-if-env-changed=STARRY_KALLSYMS_RESERVED");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let linker = format!("{out_dir}/linker.x");
    let linker_script = include_str!("linker.ld")
        .replace("__STARRY_KALLSYMS_RESERVED__", &kallsyms_reserved_size());

    std::fs::write(&linker, &linker_script).unwrap();
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
    std::fs::write(target_dir.join("linker.x"), linker_script).unwrap();
}

fn kallsyms_reserved_size() -> String {
    let size = std::env::var("STARRY_KALLSYMS_RESERVED").unwrap_or_else(|_| "8M".to_string());
    let digit_count = size.bytes().take_while(u8::is_ascii_digit).count();
    let suffix = &size[digit_count..];
    if digit_count == 0 || !matches!(suffix, "" | "K" | "M" | "G") {
        panic!("STARRY_KALLSYMS_RESERVED must be a linker size like 8M or 24576K");
    }
    size
}

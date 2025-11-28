fn main() {
    println!("cargo:rerun-if-env-changed=AXVISOR_SMP");
    println!("cargo:rerun-if-changed=linker.lds.S");

    let mut smp = 1;
    if let Ok(s) = std::env::var("AXVISOR_SMP") {
        smp = s.parse::<usize>().unwrap_or(1);
    }

    let ld_content = include_str!("linker.lds.S");
    let ld_content = ld_content.replace("%ARCH%", "riscv");
    let ld_content =
        ld_content.replace("%KERNEL_BASE%", &format!("{:#x}", 0xffff_ffc0_8020_0000usize));
    let ld_content = ld_content.replace("%SMP%", &format!("{smp}",));

    // target/<target_triple>/<mode>/build/axvisor-xxxx/out
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_path = std::path::Path::new(&out_dir).join("link.x");
    println!("cargo:rustc-link-search={out_dir}");
    std::fs::write(out_path, ld_content).unwrap();
}

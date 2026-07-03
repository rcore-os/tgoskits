fn main() {
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rustc-check-cfg=cfg(axtest)");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let linker = format!("{out_dir}/linker.x");

    std::fs::write(&linker, include_str!("linker.ld")).unwrap();
    println!("cargo:rustc-link-search={out_dir}");

    let target_dir = std::path::Path::new(&out_dir).join("../../..");
    std::fs::write(target_dir.join("linker.x"), include_str!("linker.ld")).unwrap();
}

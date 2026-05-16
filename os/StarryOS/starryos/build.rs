fn main() {
    println!("cargo:rerun-if-changed=ext_linker.ld");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let ext_linker = format!("{out_dir}/ext_linker.ld");

    std::fs::write(&ext_linker, include_str!("ext_linker.ld")).unwrap();
    println!("cargo:rustc-link-arg-bin=starryos=-T{ext_linker}");
}

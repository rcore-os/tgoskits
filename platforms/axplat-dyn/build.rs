use std::{fs, path::PathBuf};

const LINKER_SCRIPT_NAME: &str = "axplat.x";

fn main() {
    println!("cargo:rerun-if-changed=link.ld");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ld = include_str!("link.ld");
    println!("cargo:rustc-link-search={}", out_dir.display());
    let ld_content = ld.replace("{{SMP}}", &format!("{}", 16));
    fs::write(out_dir.join(LINKER_SCRIPT_NAME), ld_content).unwrap();
}

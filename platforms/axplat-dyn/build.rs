use std::{fs, io::Result, path::PathBuf};

const LINKER_SCRIPT_NAME: &str = "axplat.x";

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=link.ld");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    println!("cargo:rustc-link-search={}", out_dir.display());
    fs::copy("link.ld", out_dir.join(LINKER_SCRIPT_NAME))?;
    Ok(())
}

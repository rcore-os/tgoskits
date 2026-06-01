use std::{env, fs, io::Result, path::PathBuf};

const LINKER_SCRIPT_NAME: &str = "runtime.x";
const LINKER_TEMPLATE_NAME: &str = "runtime.ld";

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ld_content = fs::read_to_string(LINKER_TEMPLATE_NAME)?;
    let linker_path = out_dir.join(LINKER_SCRIPT_NAME);

    fs::write(&linker_path, &ld_content)?;
    println!("cargo:rustc-link-search={}", out_dir.display());

    let target_dir = out_dir.join("../../..");
    fs::write(target_dir.join(LINKER_SCRIPT_NAME), &ld_content)?;

    Ok(())
}

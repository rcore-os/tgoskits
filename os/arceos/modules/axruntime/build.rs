use std::{env, fs, io::Result, path::PathBuf};

const LINKER_TEMPLATE_NAME: &str = "runtime.ld";
const FINAL_LINKER_SCRIPT_NAME: &str = "linker.x";
const EXT_LINKER_SCRIPT_NAME: &str = "runtime.x";

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_EXT_LD");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ld_content = fs::read_to_string(LINKER_TEMPLATE_NAME)?;
    let linker_script_name = if env::var_os("CARGO_FEATURE_EXT_LD").is_some() {
        EXT_LINKER_SCRIPT_NAME
    } else {
        FINAL_LINKER_SCRIPT_NAME
    };
    let linker_path = out_dir.join(linker_script_name);

    fs::write(&linker_path, &ld_content)?;
    println!("cargo:rustc-link-search={}", out_dir.display());

    let target_dir = out_dir.join("../../..");
    fs::write(target_dir.join(linker_script_name), &ld_content)?;

    Ok(())
}

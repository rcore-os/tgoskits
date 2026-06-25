use std::{env, fs, io::Result, path::PathBuf};

const LINKER_TEMPLATE_NAME: &str = "runtime.ld";
const FINAL_LINKER_SCRIPT_NAME: &str = "linker.x";
const EXT_LINKER_SCRIPT_NAME: &str = "runtime.x";
const BUILD_INFO_NAME: &str = "build_info.rs";

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed={LINKER_TEMPLATE_NAME}");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_EXT_LD");
    println!("cargo:rerun-if-env-changed=DWARF");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ld_content = fs::read_to_string(LINKER_TEMPLATE_NAME)?.replace("%DWARF%", dwarf_sections());
    let linker_script_name = if env::var_os("CARGO_FEATURE_EXT_LD").is_some() {
        EXT_LINKER_SCRIPT_NAME
    } else {
        FINAL_LINKER_SCRIPT_NAME
    };
    let linker_path = out_dir.join(linker_script_name);

    fs::write(&linker_path, &ld_content)?;
    fs::write(out_dir.join(BUILD_INFO_NAME), build_info_source()?)?;
    println!("cargo:rustc-link-search={}", out_dir.display());

    Ok(())
}

fn build_info_source() -> Result<String> {
    let arch = env::var("CARGO_CFG_TARGET_ARCH")
        .map_err(|err| std::io::Error::other(format!("CARGO_CFG_TARGET_ARCH is not set: {err}")))?;
    let target = env::var("AX_TARGET").unwrap_or_default();
    let mode = env::var("AX_MODE").unwrap_or_default();
    Ok(build_info_source_from(&arch, &target, &mode))
}

fn build_info_source_from(arch: &str, target: &str, mode: &str) -> String {
    format!(
        "pub const ARCH: &str = {arch:?};\npub const TARGET: &str = {target:?};\npub const MODE: \
         &str = {mode:?};\n"
    )
}

fn dwarf_sections() -> &'static str {
    if env_truthy("DWARF") {
        r#"debug_abbrev : { . += SIZEOF(.debug_abbrev); }
    debug_addr : { . += SIZEOF(.debug_addr); }
    debug_aranges : { . += SIZEOF(.debug_aranges); }
    debug_info : { . += SIZEOF(.debug_info); }
    debug_line : { . += SIZEOF(.debug_line); }
    debug_line_str : { . += SIZEOF(.debug_line_str); }
    debug_ranges : { . += SIZEOF(.debug_ranges); }
    debug_rnglists : { . += SIZEOF(.debug_rnglists); }
    debug_str : { . += SIZEOF(.debug_str); }
    debug_str_offsets : { . += SIZEOF(.debug_str_offsets); }"#
    } else {
        ""
    }
}

fn env_truthy(key: &str) -> bool {
    env::var(key).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "y" | "yes" | "1" | "true" | "on"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_info_source_generates_banner_constants() {
        assert_eq!(
            build_info_source_from("riscv64", "riscv64gc-unknown-none-elf", "release"),
            concat!(
                "pub const ARCH: &str = \"riscv64\";\n",
                "pub const TARGET: &str = \"riscv64gc-unknown-none-elf\";\n",
                "pub const MODE: &str = \"release\";\n",
            )
        );
    }
}

use std::{env, fs, io, path::PathBuf};

const BUILD_INFO_NAME: &str = "build_info.rs";

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");

    let arch = env::var("CARGO_CFG_TARGET_ARCH").map_err(|err| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("CARGO_CFG_TARGET_ARCH is not set: {err}"),
        )
    })?;
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set"));

    fs::write(
        out_dir.join(BUILD_INFO_NAME),
        format!("pub const ARCH: &str = {arch:?};\n"),
    )
}

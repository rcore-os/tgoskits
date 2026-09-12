use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=src/prebuild_marker.txt");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR should be set"));
    let out_marker = out_dir.join("prebuild_marker.txt");

    let marker = fs::read_to_string("src/prebuild_marker.txt")
        .unwrap_or_else(|_| "workspace-clippy-placeholder\n".into());

    fs::write(out_marker, marker).expect("failed to write prebuild marker");
}

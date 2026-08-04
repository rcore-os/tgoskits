use std::{
    env,
    path::{Path, PathBuf},
};

fn main() {
    println!("cargo:rerun-if-changed=csrc/rknn_bridge.c");
    println!("cargo:rerun-if-env-changed=IVC_RKNN_BRIDGE_LIB_DIR");
    println!("cargo:rerun-if-env-changed=IVC_RKNN_RUNTIME_LIB_DIR");

    if env::var_os("CARGO_FEATURE_RKNN").is_none()
        || env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("aarch64")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu")
    {
        return;
    }

    let bridge_dir = required_directory("IVC_RKNN_BRIDGE_LIB_DIR");
    let runtime_dir = required_directory("IVC_RKNN_RUNTIME_LIB_DIR");
    require_file(
        "RKNN bridge archive",
        &bridge_dir.join("libivc_rknn_bridge.a"),
    );
    require_file("RKNN runtime", &runtime_dir.join("librknnrt.so"));

    println!("cargo:rustc-link-search=native={}", bridge_dir.display());
    println!("cargo:rustc-link-lib=static=ivc_rknn_bridge");
    println!("cargo:rustc-link-search=native={}", runtime_dir.display());
    println!("cargo:rustc-link-lib=dylib=rknnrt");
    println!("cargo:rustc-link-lib=dylib=dl");
    println!("cargo:rustc-link-lib=dylib=pthread");
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/lib");
}

fn required_directory(name: &str) -> PathBuf {
    let path = env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} must identify the frozen RKNN build input"));
    assert!(
        path.is_dir(),
        "{name} is not a directory: {}",
        path.display()
    );
    path
}

fn require_file(label: &str, path: &Path) {
    assert!(path.is_file(), "{label} is missing: {}", path.display());
}

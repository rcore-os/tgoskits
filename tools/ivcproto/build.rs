use std::{
    env,
    path::{Path, PathBuf},
};

fn main() {
    println!("cargo:rerun-if-changed=csrc/rknn_bridge.c");
    println!("cargo:rerun-if-changed=csrc/ort_bridge.cpp");
    println!("cargo:rerun-if-env-changed=IVC_RKNN_BRIDGE_LIB_DIR");
    println!("cargo:rerun-if-env-changed=IVC_RKNN_RUNTIME_LIB_DIR");
    println!("cargo:rerun-if-env-changed=IVC_ORT_BRIDGE_LIB_DIR");
    println!("cargo:rerun-if-env-changed=IVC_ORT_RUNTIME_LIB_DIR");

    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("aarch64")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu")
    {
        return;
    }

    let has_rknn = env::var_os("CARGO_FEATURE_RKNN").is_some();
    let has_ort = env::var_os("CARGO_FEATURE_ONNXRUNTIME").is_some();
    if has_rknn {
        configure_rknn();
    }
    if has_ort {
        configure_ort();
    }
    if has_rknn || has_ort {
        println!("cargo:rustc-link-lib=dylib=dl");
        println!("cargo:rustc-link-lib=dylib=pthread");
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/lib");
    }
}

fn configure_rknn() {
    let bridge_dir = required_directory("IVC_RKNN_BRIDGE_LIB_DIR", "frozen RKNN bridge");
    let runtime_dir = required_directory("IVC_RKNN_RUNTIME_LIB_DIR", "frozen RKNN runtime");
    require_file(
        "RKNN bridge archive",
        &bridge_dir.join("libivc_rknn_bridge.a"),
    );
    require_file("RKNN runtime", &runtime_dir.join("librknnrt.so"));

    println!("cargo:rustc-link-search=native={}", bridge_dir.display());
    println!("cargo:rustc-link-lib=static=ivc_rknn_bridge");
    println!("cargo:rustc-link-search=native={}", runtime_dir.display());
    println!("cargo:rustc-link-lib=dylib=rknnrt");
}

fn configure_ort() {
    let bridge_dir = required_directory("IVC_ORT_BRIDGE_LIB_DIR", "frozen ORT bridge");
    let runtime_dir = required_directory("IVC_ORT_RUNTIME_LIB_DIR", "frozen ORT runtime");
    require_file(
        "ORT bridge archive",
        &bridge_dir.join("libivc_ort_bridge.a"),
    );
    require_file("ORT runtime", &runtime_dir.join("libonnxruntime.so"));

    println!("cargo:rustc-link-search=native={}", bridge_dir.display());
    println!("cargo:rustc-link-lib=static=ivc_ort_bridge");
    println!("cargo:rustc-link-search=native={}", runtime_dir.display());
    println!("cargo:rustc-link-lib=dylib=onnxruntime");
    println!("cargo:rustc-link-lib=dylib=stdc++");
}

fn required_directory(name: &str, purpose: &str) -> PathBuf {
    let path = env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} must identify the {purpose}"));
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

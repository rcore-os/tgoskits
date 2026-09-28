use std::{
    env,
    path::{Path, PathBuf},
};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(akars_no_tpu)");
    println!("cargo:rerun-if-env-changed=AKARS_TPU_SDK_DIR");

    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("riscv64") {
        return;
    }

    let manifest_dir = env_path("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let default_sdk = manifest_dir.join("../thirdparty/tpu-sdk-sg200x");
    let tpu_sdk = env_path("AKARS_TPU_SDK_DIR").unwrap_or(default_sdk);
    let tpu_sdk_lib = tpu_sdk.join("lib");

    require_dir("TPU SDK library", &tpu_sdk_lib);
    println!("cargo:rustc-link-search=native={}", tpu_sdk_lib.display());
    println!("cargo:rustc-link-lib=dylib=cviruntime");
    println!("cargo:rustc-link-lib=dylib=cvikernel");
    println!("cargo:rustc-link-lib=dylib=stdc++");
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn require_dir(label: &str, path: &Path) {
    assert!(
        path.is_dir(),
        "{label} directory does not exist: {}",
        path.display()
    );
}

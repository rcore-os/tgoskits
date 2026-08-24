use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=NCNN_PREFIX");
    println!("cargo:rerun-if-changed=src/ncnn/adapter.cc");

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo must set target arch");
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo must set target OS");
    if target_arch != "aarch64" || target_os != "linux" {
        return;
    }

    let prefix = PathBuf::from(
        env::var_os("NCNN_PREFIX")
            .expect("NCNN_PREFIX is required for the AArch64 StarryOS endpoint"),
    );
    let include = prefix.join("include");
    let library = prefix.join("lib");
    if !include.join("ncnn/net.h").is_file() || !library.join("libncnn.a").is_file() {
        panic!(
            "NCNN_PREFIX={} must contain include/ncnn/net.h and lib/libncnn.a",
            prefix.display()
        );
    }

    cc::Build::new()
        .cpp(true)
        .file("src/ncnn/adapter.cc")
        .include(include)
        .flag_if_supported("-std=c++11")
        .flag_if_supported("-fno-exceptions")
        .flag_if_supported("-fno-rtti")
        .compile("starry_task3_ncnn_adapter");

    println!("cargo:rustc-link-search=native={}", library.display());
    println!("cargo:rustc-link-lib=static=ncnn");
    println!("cargo:rustc-link-lib=stdc++");
    println!("cargo:rustc-link-lib=gcc");
    println!("cargo:rustc-link-lib=m");
    println!("cargo:rustc-link-lib=pthread");
}

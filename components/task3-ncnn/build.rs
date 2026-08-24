use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=NCNN_PREFIX");
    println!("cargo:rerun-if-env-changed=NCNN_SOURCE");
    let Some(prefix) = env::var_os("NCNN_PREFIX") else {
        println!("cargo:warning=task3-ncnn disabled: NCNN_PREFIX is not set");
        return;
    };
    let prefix = PathBuf::from(prefix);
    let include = prefix.join("include");
    let lib = prefix.join("lib");
    if !include.join("ncnn/net.h").is_file() || !lib.join("libncnn.a").is_file() {
        panic!(
            "NCNN_PREFIX={} does not contain include/ncnn/net.h and lib/libncnn.a",
            prefix.display()
        );
    }

    cc::Build::new()
        .cpp(true)
        .file("src/adapter.cc")
        .include(&include)
        .flag_if_supported("-std=c++11")
        .flag_if_supported("-fno-exceptions")
        .flag_if_supported("-fno-rtti")
        .compile("task3_ncnn_adapter");

    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=static=ncnn");
    println!("cargo:rustc-link-lib=stdc++");
    println!("cargo:rustc-link-lib=gcc");
    println!("cargo:rustc-link-lib=m");
    println!("cargo:rustc-link-lib=pthread");
}

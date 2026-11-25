use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let adapter_dir = manifest_dir.join("../lib");
    let sources = [
        adapter_dir.join("discovery.c"),
        adapter_dir.join("backend_polling.c"),
        adapter_dir.join("errors.c"),
    ];

    println!("cargo:rerun-if-changed=build.rs");
    for source in &sources {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    for header in ["ivshmem.h", "ivshmem_internal.h"] {
        println!(
            "cargo:rerun-if-changed={}",
            adapter_dir.join(header).display()
        );
    }

    cc::Build::new()
        .include(&adapter_dir)
        .std("c11")
        .warnings(true)
        .flag_if_supported("-Werror=implicit-function-declaration")
        .files(sources)
        .compile("ivshmem_adapter");
}

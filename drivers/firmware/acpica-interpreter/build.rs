use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create generated ACPICA include directory");
    let mut entries: Vec<_> = fs::read_dir(from)
        .expect("read vendored ACPICA directory")
        .map(|entry| entry.expect("read ACPICA directory entry").path())
        .collect();
    entries.sort();
    for source in entries {
        let destination = to.join(source.file_name().expect("ACPICA path has a file name"));
        if source.is_dir() {
            copy_tree(&source, &destination);
        } else {
            fs::copy(source, destination).expect("copy generated ACPICA header");
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-changed=c");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CC");

    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo supplies target architecture");
    assert_eq!(
        arch, "x86_64",
        "acpica-interpreter currently supports x86_64 only"
    );
    let pointer_width =
        env::var("CARGO_CFG_TARGET_POINTER_WIDTH").expect("Cargo supplies target pointer width");
    assert_eq!(
        pointer_width, "64",
        "acpica-interpreter requires a 64-bit pointer ABI"
    );
    let target = env::var("TARGET").expect("Cargo supplies target triple");
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo supplies target OS");
    assert_ne!(
        target_os, "windows",
        "acpica-interpreter's current C ABI configuration requires LP64, not LLP64"
    );

    let include =
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR")).join("include");
    copy_tree(Path::new("vendor/include"), &include);
    let platform_header = include.join("platform/acenv.h");
    let original = fs::read_to_string(&platform_header).expect("read generated ACPICA header");
    let needle = "#if defined(_LINUX) || defined(__linux__)";
    assert_eq!(original.matches(needle).count(), 1);
    fs::write(
        platform_header,
        original.replacen(
            needle,
            "#if defined(__TGOSKITS__)\n#include \"actgoskits.h\"\n#elif defined(_LINUX) || \
             defined(__linux__)",
            1,
        ),
    )
    .expect("select TGOSKits ACPICA platform configuration");
    fs::copy("c/actgoskits.h", include.join("platform/actgoskits.h"))
        .expect("copy TGOSKits ACPICA platform configuration");

    let mut build = cc::Build::new();
    build
        .target(&target)
        .include(&include)
        .define("__TGOSKITS__", None)
        .flag("-ffreestanding")
        .flag("-fno-builtin")
        .flag("-fno-stack-protector")
        .flag("-std=gnu11")
        .warnings(false);

    if target_os == "none" {
        // The x86_64 kernel ABI has no red zone and kernel code must not use
        // SIMD registers. cc resolves the compiler using Cargo's target settings.
        build
            .flag("-mno-red-zone")
            .flag("-mno-sse")
            .flag("-mno-sse2")
            .flag("-mno-mmx")
            .flag("-mcmodel=large")
            .pic(false);
    }

    if env::var_os("CARGO_FEATURE_HOST_TEST").is_some() {
        build.define("ACPICA_INTERPRETER_HOST_TEST", None);
    }

    for component in [
        "dispatcher",
        "events",
        "executer",
        "hardware",
        "namespace",
        "parser",
        "resources",
        "tables",
        "utilities",
    ] {
        let mut sources: Vec<_> = fs::read_dir(format!("vendor/components/{component}"))
            .expect("read ACPICA component directory")
            .map(|entry| entry.expect("read ACPICA source entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "c"))
            .collect();
        sources.retain(|path| {
            !matches!(
                path.file_stem().and_then(|name| name.to_str()),
                Some("rsdump" | "rsdumpinfo")
            )
        });
        sources.sort();
        build.files(sources);
    }

    build
        .file("c/abi.c")
        .file("c/bridge.c")
        .compile("acpica_interpreter");
}

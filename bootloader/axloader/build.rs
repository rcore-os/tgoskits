struct Target {
    arch_cfg: &'static str,
    uefi_target: &'static str,
}

const UEFI_TARGETS: &[Target] = &[
    Target {
        arch_cfg: "x86_64",
        uefi_target: "x86_64-unknown-uefi",
    },
    Target {
        arch_cfg: "riscv64",
        uefi_target: "riscv64-unknown-uefi",
    },
];

fn main() {
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() != Some(std::ffi::OsStr::new("uefi")) {
        return;
    }

    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    let Some(target_info) = UEFI_TARGETS
        .iter()
        .find(|target_info| target_info.uefi_target == target)
    else {
        panic!("unsupported axloader UEFI target `{target}`");
    };

    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok(target_info.arch_cfg) {
        panic!(
            "axloader target `{}` requires target_arch `{}`, got `{}`",
            target_info.uefi_target,
            target_info.arch_cfg,
            std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "unknown".to_string())
        );
    }

    if target_info.arch_cfg == "riscv64" {
        let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let script = root.join("src/riscv64_uefi/link.ld");
        println!("cargo::rerun-if-changed={}", script.display());
        println!("cargo::rerun-if-changed=src/riscv64_uefi/entry.S");
        println!("cargo::rerun-if-changed=src/riscv64_uefi/linker.py");
        println!("cargo::rustc-link-arg-bin=axloader=-T{}", script.display());
    }
}

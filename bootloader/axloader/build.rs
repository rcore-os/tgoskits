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
        arch_cfg: "loongarch64",
        uefi_target: "loongarch64-unknown-uefi",
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
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "unknown".to_string());
    let Some(target_info) = UEFI_TARGETS
        .iter()
        .find(|target_info| target_info.uefi_target == target || target_info.arch_cfg == arch)
    else {
        panic!("unsupported axloader UEFI target `{target}`");
    };

    if arch != target_info.arch_cfg {
        panic!(
            "axloader target `{}` requires target_arch `{}`, got `{}`",
            target_info.uefi_target, target_info.arch_cfg, arch
        );
    }

    if target_info.arch_cfg == "loongarch64" {
        let out_dir = std::path::PathBuf::from(
            std::env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR"),
        );
        let linker_script = out_dir.join("axloader-loongarch64.ld");
        std::fs::write(&linker_script, include_str!("src/arch/loongarch64/link.ld"))
            .expect("failed to write the LoongArch axloader linker script");
        println!(
            "cargo:rerun-if-changed={}",
            std::path::Path::new("src/arch/loongarch64/link.ld").display()
        );
        for binary in ["axloader", "axloader-launcher"] {
            println!(
                "cargo:rustc-link-arg-bin={binary}=-T{}",
                linker_script.display()
            );
            println!("cargo:rustc-link-arg-bin={binary}=-pie");
            println!("cargo:rustc-link-arg-bin={binary}=--gc-sections");
            println!("cargo:rustc-link-arg-bin={binary}=-znorelro");
            println!("cargo:rustc-link-arg-bin={binary}=-znostart-stop-gc");
        }
    } else if target_info.arch_cfg == "riscv64" {
        let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let script = root.join("src/arch/riscv64/link.ld");
        println!("cargo::rerun-if-changed={}", script.display());
        println!("cargo::rerun-if-changed=src/arch/riscv64/linker.py");
        for binary in ["axloader", "axloader-launcher"] {
            println!("cargo::rustc-link-arg-bin={binary}=-T{}", script.display());
        }
    }
}

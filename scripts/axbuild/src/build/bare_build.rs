use std::{collections::HashMap, path::Path};

use crate::context::arch_spec_for_target;

pub(crate) const RISCV64_UEFI_TARGET: &str = "riscv64-unknown-uefi";

/// Resolved target specification, compiler defaults and build environment.
pub(crate) struct CargoBuildTarget {
    pub(crate) target: String,
    pub(crate) cargo_args: Vec<String>,
    pub(crate) rustflags: Vec<String>,
    pub(crate) env: HashMap<String, String>,
}

/// Resolves one of the four workspace-owned freestanding target specifications.
pub(crate) fn bare_build_target_for(target: &str) -> Option<CargoBuildTarget> {
    let arch = arch_spec_for_target(target)?;
    Some(CargoBuildTarget {
        target: Path::new("scripts/targets/bare")
            .join(format!("{target}.json"))
            .display()
            .to_string(),
        cargo_args: vec![
            "-Z".to_string(),
            "json-target-spec".to_string(),
            "-Z".to_string(),
            "build-std=core,alloc".to_string(),
        ],
        // LLVM 23 otherwise limits strict-alignment memcpy lowering to four
        // stores. Raising only this cost limit preserves early-boot alignment
        // requirements while keeping ordinary kernel aggregates inline.
        rustflags: if arch.arch == "aarch64" {
            vec!["-Cllvm-args=--max-store-memcpy=16".to_string()]
        } else {
            Vec::new()
        },
        env: HashMap::from([(
            "CARGO_UNSTABLE_JSON_TARGET_SPEC".to_string(),
            "true".to_string(),
        )]),
    })
}

/// Resolves native EFI targets whose PE header is embedded in an ELF link.
pub(crate) fn uefi_build_target_for(target: &str) -> Option<CargoBuildTarget> {
    if target != RISCV64_UEFI_TARGET {
        return None;
    }
    Some(CargoBuildTarget {
        target: Path::new("scripts/targets/uefi")
            .join(format!("{target}.json"))
            .display()
            .to_string(),
        cargo_args: vec![
            "-Z".to_string(),
            "json-target-spec".to_string(),
            "-Z".to_string(),
            "build-std=core,alloc".to_string(),
            "--config".to_string(),
            "profile.release.lto=false".to_string(),
        ],
        // The compiler requires is-like-windows for os=uefi, but its embedded
        // bitcode assembly uses COFF section flags that ELF cannot represent.
        rustflags: vec!["-Cembed-bitcode=no".to_string()],
        env: HashMap::from([(
            "CARGO_UNSTABLE_JSON_TARGET_SPEC".to_string(),
            "true".to_string(),
        )]),
    })
}

/// Resolves a freestanding target while preserving external built-in targets.
pub(crate) fn freestanding_build_target_for(target: &str) -> CargoBuildTarget {
    bare_build_target_for(target).unwrap_or_else(|| CargoBuildTarget {
        target: target.to_string(),
        cargo_args: vec!["-Z".to_string(), "build-std=core,alloc".to_string()],
        rustflags: Vec::new(),
        env: HashMap::new(),
    })
}

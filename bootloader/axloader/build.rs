struct Target {
    arch_cfg: &'static str,
    uefi_target: &'static str,
}

const UEFI_TARGETS: &[Target] = &[Target {
    arch_cfg: "x86_64",
    uefi_target: "x86_64-unknown-uefi",
}];

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
}

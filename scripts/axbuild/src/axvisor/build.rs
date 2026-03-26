use std::path::{Path, PathBuf};

use ostool::build::config::Cargo;

use crate::context::{ResolvedAxvisorRequest, arch_for_target_checked};

pub type AxvisorBuildInfo = crate::arceos::build::ArceosBuildInfo;
pub use crate::arceos::build::LogLevel;

pub const AXVISOR_PACKAGE: &str = "axvisor";

impl AxvisorBuildInfo {
    pub fn default_axvisor_for_target(target: &str) -> Self {
        let mut build_info = Self::default_for_target(target);
        build_info.features.clear();
        build_info
    }
}

pub fn resolve_build_info_path(
    workspace_root: &Path,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit_path {
        return Ok(path);
    }

    let _ = arch_for_target_checked(target)?;
    Ok(crate::arceos::build::resolve_build_info_path_in_dir(
        &workspace_root.join("os/axvisor"),
        target,
    ))
}

pub fn load_build_info(request: &ResolvedAxvisorRequest) -> anyhow::Result<AxvisorBuildInfo> {
    crate::arceos::build::load_or_create_build_info(&request.build_info_path, || {
        AxvisorBuildInfo::default_axvisor_for_target(&request.target)
    })
}

pub fn load_cargo_config(request: &ResolvedAxvisorRequest) -> anyhow::Result<Cargo> {
    to_cargo_config(load_build_info(request)?, request)
}

pub fn default_qemu_args(arch: &str) -> anyhow::Result<Vec<String>> {
    let mut args = vec!["-m".to_string(), "2G".to_string()];
    match arch {
        "aarch64" => args.extend([
            "-machine".to_string(),
            "virt,virtualization=on,gic-version=3".to_string(),
        ]),
        "x86_64" => args.extend([
            "-accel".to_string(),
            "kvm".to_string(),
            "-cpu".to_string(),
            "host".to_string(),
        ]),
        "riscv64" | "loongarch64" => {}
        _ => anyhow::bail!(
            "unsupported Axvisor architecture `{arch}`; expected one of aarch64, x86_64, riscv64, \
             loongarch64"
        ),
    }
    Ok(args)
}

pub fn to_cargo_config(
    build_info: AxvisorBuildInfo,
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<Cargo> {
    let mut cargo = build_info.into_prepared_base_cargo_config(
        &request.package,
        &request.target,
        request.plat_dyn,
    )?;
    patch_axvisor_cargo_config(&mut cargo, request)?;
    Ok(cargo)
}

fn patch_axvisor_cargo_config(
    cargo: &mut Cargo,
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<()> {
    cargo.package = request.package.clone();
    cargo.target = request.target.clone();
    cargo
        .env
        .insert("AX_ARCH".to_string(), request.arch.clone());
    cargo
        .env
        .insert("AX_TARGET".to_string(), request.target.clone());

    if !request.vmconfigs.is_empty() {
        let joined = std::env::join_paths(&request.vmconfigs)
            .map_err(|e| anyhow!("failed to join vmconfig paths: {e}"))?;
        cargo.env.insert(
            "AXVISOR_VM_CONFIGS".to_string(),
            joined.to_string_lossy().into_owned(),
        );
    }

    cargo.features.sort();
    cargo.features.dedup();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn request(path: PathBuf, arch: &str, target: &str) -> ResolvedAxvisorRequest {
        ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            arch: arch.to_string(),
            target: target.to_string(),
            plat_dyn: None,
            build_info_path: path,
            qemu_config: None,
            vmconfigs: vec![],
        }
    }

    #[test]
    fn resolve_build_info_path_uses_default_axvisor_location() {
        let root = tempdir().unwrap();
        let path =
            resolve_build_info_path(root.path(), "aarch64-unknown-none-softfloat", None).unwrap();

        assert_eq!(
            path,
            root.path()
                .join("os/axvisor/.build-aarch64-unknown-none-softfloat.toml")
        );
    }

    #[test]
    fn resolve_build_info_path_prefers_explicit_path() {
        let root = tempdir().unwrap();
        let explicit = root.path().join("custom/build.toml");
        let path =
            resolve_build_info_path(root.path(), "x86_64-unknown-none", Some(explicit.clone()))
                .unwrap();

        assert_eq!(path, explicit);
    }

    #[test]
    fn load_build_info_writes_default_template_when_missing() {
        let root = tempdir().unwrap();
        let path = root
            .path()
            .join("os/axvisor/.build-aarch64-unknown-none-softfloat.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let build_info = load_build_info(&request(
            path.clone(),
            "aarch64",
            "aarch64-unknown-none-softfloat",
        ))
        .unwrap();

        assert_eq!(
            build_info,
            AxvisorBuildInfo::default_axvisor_for_target("aarch64-unknown-none-softfloat")
        );
        assert!(path.exists());
    }

    #[test]
    fn load_cargo_config_injects_vmconfigs() {
        let root = tempdir().unwrap();
        let config_path = root.path().join(".build.toml");
        fs::write(
            &config_path,
            r#"
env = {}
features = ["fs", "ept-level-4"]
log = "Info"
plat_dyn = true
"#,
        )
        .unwrap();

        let cargo = load_cargo_config(&ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: Some(true),
            build_info_path: config_path,
            qemu_config: None,
            vmconfigs: vec![root.path().join("a.toml"), root.path().join("b.toml")],
        })
        .unwrap();

        assert_eq!(cargo.package, AXVISOR_PACKAGE);
        assert_eq!(cargo.target, "aarch64-unknown-none-softfloat");
        assert_eq!(
            cargo.env.get("AX_ARCH").map(String::as_str),
            Some("aarch64")
        );
        assert_eq!(
            cargo.env.get("AX_TARGET").map(String::as_str),
            Some("aarch64-unknown-none-softfloat")
        );
        assert!(cargo.env.contains_key("AXVISOR_VM_CONFIGS"));
    }

    #[test]
    fn default_qemu_args_enable_virtualization_support() {
        assert_eq!(
            default_qemu_args("aarch64").unwrap(),
            vec![
                "-m".to_string(),
                "2G".to_string(),
                "-machine".to_string(),
                "virt,virtualization=on,gic-version=3".to_string()
            ]
        );
        assert_eq!(
            default_qemu_args("x86_64").unwrap(),
            vec![
                "-m".to_string(),
                "2G".to_string(),
                "-accel".to_string(),
                "kvm".to_string(),
                "-cpu".to_string(),
                "host".to_string()
            ]
        );
    }
}

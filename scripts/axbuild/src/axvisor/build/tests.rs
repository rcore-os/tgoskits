use std::{
    fs,
    path::{Path, PathBuf},
};

use tempfile::tempdir;

use super::*;

fn workspace() -> WorkspaceContext {
    WorkspaceContext::discover(None).unwrap()
}

fn request(path: PathBuf, arch: &str, target: &str) -> ResolvedAxvisorRequest {
    let workspace_root = path.parent().unwrap_or_else(|| Path::new("."));
    let input = crate::build::load_config_input::<AxvisorConfig>(workspace_root, Some(&path), None)
        .unwrap();
    request_from_input(input, arch, target, workspace_root.join("os/axvisor"))
}

fn request_from_input(
    input: crate::build::ConfigInput<AxvisorConfig>,
    arch: &str,
    target: &str,
    axvisor_dir: PathBuf,
) -> ResolvedAxvisorRequest {
    let mut config = input.config;
    config.common.target = Some(target.to_owned());
    let runtime_env = ax_config::RuntimeEnv::default();
    let runtime = ax_config::resolve_runtime(
        config.common.max_cpu_num,
        config.common.runtime,
        &runtime_env,
    )
    .unwrap();
    ResolvedAxvisorRequest {
        package: AXVISOR_PACKAGE.to_owned(),
        axvisor_dir,
        arch: arch.to_owned(),
        target: target.to_owned(),
        smp: None,
        debug: false,
        source: input.source,
        config,
        runtime,
        runtime_env,
        environment_features: Vec::new(),
        qemu_config: None,
        uboot_config: None,
        vmconfigs: Vec::new(),
    }
}

#[test]
fn guest_resources_do_not_change_cargo_build_identity() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    let vmconfigs = vec![root.path().join("a.toml"), root.path().join("b.toml")];
    for vmconfig in &vmconfigs {
        fs::write(vmconfig, "[kernel]\n").unwrap();
    }
    fs::write(
        &config_path,
        r#"
features = []
log = "Info"
"#,
    )
    .unwrap();

    let mut build_request = request(
        config_path.clone(),
        "aarch64",
        "aarch64-unknown-none-softfloat",
    );
    build_request.vmconfigs = vmconfigs.clone();
    let cargo = load_cargo_config(&build_request, &workspace()).unwrap();

    assert_eq!(cargo.package, AXVISOR_PACKAGE);
    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/aarch64-unknown-linux-musl.json")
    );
    assert_eq!(
        cargo.env.get("AX_ARCH").map(String::as_str),
        Some("aarch64")
    );
    assert_eq!(
        cargo.env.get("AX_TARGET").map(String::as_str),
        Some("aarch64-unknown-none-softfloat")
    );
    let mut alternate = request(
        root.path().join(".build.toml"),
        "aarch64",
        "aarch64-unknown-none-softfloat",
    );
    // Cargo consumes the confirmed request, not a second read of its source.
    fs::remove_file(root.path().join(".build.toml")).unwrap();
    alternate.vmconfigs = vec![root.path().join("missing-resource.toml")];
    let other = load_cargo_config(&alternate, &workspace()).unwrap();
    assert_eq!(cargo.env, other.env);
    assert_eq!(cargo.features, other.features);
    assert_eq!(cargo.args, other.args);
    assert_eq!(
        cargo
            .args
            .windows(2)
            .find_map(|window| (window[0] == "--bin").then_some(window[1].as_str())),
        Some("axvisor")
    );
    assert!(cargo.features.is_empty());
    let extra_config = cargo.extra_config.as_ref().expect("std cargo config");
    assert!(
        fs::read_to_string(extra_config)
            .unwrap()
            .contains("-Zstack-protector=strong")
    );
}

#[test]
fn load_cargo_config_rejects_explicit_x86_backend_features() {
    for feature in ["vmx", "svm"] {
        let root = tempdir().unwrap();
        let config_path = root.path().join(format!("build-x86_64-{feature}.toml"));
        fs::write(
            &config_path,
            format!(
                r#"
features = ["{feature}"]
log = "Info"
"#
            ),
        )
        .unwrap();

        let err = load_cargo_config(
            &request(config_path, "x86_64", "x86_64-unknown-none"),
            &workspace(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("selected from CPU capabilities"));
        assert!(err.to_string().contains(&format!("`{feature}`")));
    }
}

#[test]
fn load_cargo_config_uses_static_preset_without_materializing_a_toml() {
    let root = tempdir().unwrap();
    let path = root
        .path()
        .join("os/axvisor/.build-x86_64-unknown-none.toml");
    let input =
        crate::build::load_config_input::<AxvisorConfig>(root.path(), None, Some("qemu-x86_64"))
            .unwrap();
    let request = request_from_input(
        input,
        "x86_64",
        "x86_64-unknown-none",
        root.path().join("os/axvisor"),
    );
    let cargo = load_cargo_config(&request, &workspace()).unwrap();

    assert!(!path.exists());
    assert!(!cargo.features.contains(&"fs".to_string()));
    assert!(!cargo.features.contains(&"vmx".to_string()));
    assert!(!cargo.features.contains(&"plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"axvm/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-driver/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-std/x86-pc".to_string()));
    assert!(!cargo.features.contains(&"ax-hal/x86-pc".to_string()));
}

#[test]
fn load_cargo_config_rejects_direct_axplat_dyn_feature() {
    let root = tempdir().unwrap();
    let config_path = root.path().join(".build.toml");
    fs::write(
        &config_path,
        r#"
features = ["axplat-dyn/efi"]
log = "Info"
"#,
    )
    .unwrap();

    let err = load_cargo_config(
        &request(
            config_path,
            "loongarch64",
            "loongarch64-unknown-none-softfloat",
        ),
        &workspace(),
    )
    .unwrap_err();

    assert!(err.to_string().contains("dynamic platform features"));
    assert!(err.to_string().contains("axplat-dyn/efi"));
}

use super::*;

fn load_std_cargo_config(path: &Path) -> toml::Table {
    toml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn std_cargo_config_uses_linux_musl_wrapper_with_plain_std_build() {
    let fake_dir = std_fake_lib_dir("x86_64-unknown-linux-musl").unwrap();
    let wrapper = std_linker_wrapper_path("x86_64-unknown-linux-musl", &fake_dir).unwrap();
    let config = std_cargo_config_path("x86_64-unknown-linux-musl", &wrapper, &[]).unwrap();
    let config = fs::read_to_string(config).unwrap();
    let parsed: toml::Table = toml::from_str(&config).unwrap();

    assert_eq!(
        parsed["unstable"]["build-std"].as_array().unwrap(),
        &vec![
            toml::Value::String("std".to_string()),
            toml::Value::String("panic_abort".to_string())
        ]
    );
    assert_eq!(
        parsed["unstable"]["build-std-features"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        parsed["target"]["x86_64-unknown-linux-musl"]["linker"].as_str(),
        Some(wrapper.display().to_string().as_str())
    );
    assert!(!config.contains("--cfg"));
    assert!(!config.contains("--check-cfg"));
    assert!(!config.contains("relocation-model"));
    assert!(!config.contains("code-model"));
    assert!(!config.contains("--cfg"));
    assert!(!config.contains("--check-cfg"));
}

#[test]
fn std_cargo_config_leaves_kernel_codegen_to_target_spec() {
    let fake_dir = std_fake_lib_dir("loongarch64-unknown-linux-musl").unwrap();
    let wrapper = std_linker_wrapper_path("loongarch64-unknown-linux-musl", &fake_dir).unwrap();
    let config = std_cargo_config_path("loongarch64-unknown-linux-musl", &wrapper, &[]).unwrap();
    let config = fs::read_to_string(config).unwrap();

    assert!(!config.contains("--cfg"));
    assert!(!config.contains("--check-cfg"));
    assert!(!config.contains("relocation-model"));
    assert!(!config.contains("code-model"));
}

#[test]
fn std_cargo_config_uses_dynamic_link_mode_without_codegen_override() {
    let fake_dir = std_fake_lib_dir("aarch64-unknown-linux-musl").unwrap();
    let wrapper = std_linker_wrapper_path("aarch64-unknown-linux-musl", &fake_dir).unwrap();
    let app_config = std_cargo_config_path("aarch64-unknown-linux-musl", &wrapper, &[]).unwrap();

    let config = fs::read_to_string(app_config).unwrap();
    assert!(!config.contains("--cfg"));
    assert!(!config.contains("--check-cfg"));
    assert!(!config.contains("relocation-model"));
    assert!(!config.contains("code-model"));
}

#[test]
fn std_cargo_config_serializes_structured_toml_fields() {
    let fake_dir = std_fake_lib_dir("x86_64-unknown-linux-musl").unwrap();
    let wrapper = std_linker_wrapper_path("x86_64-unknown-linux-musl", &fake_dir).unwrap();
    let path = std_cargo_config_path(
        "x86_64-unknown-linux-musl",
        &wrapper,
        &["--cfg".to_string(), r#"feature="quote\slash""#.to_string()],
    )
    .unwrap();

    let config = load_std_cargo_config(&path);
    let unstable = config.get("unstable").unwrap().as_table().unwrap();
    assert_eq!(
        unstable.get("build-std").unwrap().as_array().unwrap(),
        &vec![
            toml::Value::String("std".to_string()),
            toml::Value::String("panic_abort".to_string())
        ]
    );
    assert_eq!(
        unstable
            .get("build-std-features")
            .unwrap()
            .as_array()
            .unwrap(),
        &Vec::<toml::Value>::new()
    );

    let profile = config
        .get("profile")
        .unwrap()
        .get("release")
        .unwrap()
        .as_table()
        .unwrap();
    assert_eq!(profile.get("lto").unwrap().as_bool(), Some(false));
    assert_eq!(profile.get("panic").unwrap().as_str(), Some("abort"));

    let target = config
        .get("target")
        .unwrap()
        .get("x86_64-unknown-linux-musl")
        .unwrap()
        .as_table()
        .unwrap();
    assert_eq!(
        target.get("linker").unwrap().as_str(),
        Some(wrapper.display().to_string().as_str())
    );
    assert_eq!(
        target
            .get("rustflags")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["--cfg", r#"feature="quote\slash""#]
    );
}

#[test]
fn std_cargo_config_serializes_empty_rustflags_as_empty_array() {
    let fake_dir = std_fake_lib_dir("riscv64gc-unknown-linux-musl").unwrap();
    let wrapper = std_linker_wrapper_path("riscv64gc-unknown-linux-musl", &fake_dir).unwrap();
    let path = std_cargo_config_path("riscv64gc-unknown-linux-musl", &wrapper, &[]).unwrap();

    let config = load_std_cargo_config(&path);
    let target = config
        .get("target")
        .unwrap()
        .get("riscv64gc-unknown-linux-musl")
        .unwrap()
        .as_table()
        .unwrap();
    assert_eq!(
        target.get("rustflags").unwrap().as_array().unwrap().len(),
        0
    );
}

#[test]
fn std_linker_wrapper_filters_crt_and_replaces_fixed_libs() {
    let fake_dir = std_fake_lib_dir("x86_64-unknown-linux-musl").unwrap();
    let wrapper = std_linker_wrapper_path("x86_64-unknown-linux-musl", &fake_dir).unwrap();
    let wrapper = fs::read_to_string(wrapper).unwrap();

    assert!(wrapper.contains("rust-lld"));
    assert!(wrapper.contains("link_search_dirs=()"));
    assert!(wrapper.contains("archive_args=()"));
    assert!(wrapper.contains("add_link_search_dir"));
    assert!(wrapper.contains("append_lld_arg"));
    assert!(wrapper.contains("flush_archive_group"));
    assert!(wrapper.contains("--start-group"));
    assert!(wrapper.contains("--end-group"));
    assert!(wrapper.contains("find_linker_script"));
    assert!(wrapper.contains("failed to find linker.x in current linker search dirs"));
    assert!(!wrapper.contains("entry_symbol="));
    assert!(!wrapper.contains("link_mode_args="));
    assert!(!wrapper.contains("dynamic_platform="));
    assert!(wrapper.contains("crtbegin"));
    assert!(wrapper.contains("static-pie"));
    assert!(wrapper.contains("-flavor"));
    assert!(wrapper.contains("-T*"));
    assert!(wrapper.contains("--eh-frame-hdr"));
    assert!(wrapper.contains("relro"));
    assert!(wrapper.contains("noexecstack"));
    assert!(!wrapper.contains("-znorelro"));
    assert!(!wrapper.contains("--gc-sections"));
    assert!(!wrapper.contains("-znostart-stop-gc"));
    assert!(wrapper.contains("libc.a"));
    assert!(wrapper.contains("libunwind.a"));
    assert!(wrapper.contains("-lgcc_s|-lgcc"));
    assert!(!wrapper.contains("--whole-archive"));
    assert!(!wrapper.contains("\"-u\""));
    assert!(!wrapper.contains("_start"));
}

#[test]
fn std_linker_wrapper_uses_explicit_dynamic_platform_mode() {
    let fake_dir = std_fake_lib_dir("aarch64-unknown-linux-musl").unwrap();
    let wrapper = std_linker_wrapper_path("aarch64-unknown-linux-musl", &fake_dir).unwrap();
    let wrapper = fs::read_to_string(wrapper).unwrap();

    assert!(wrapper.contains("find_linker_script"));
    assert!(!wrapper.contains("latest_build_output_script axplat.x"));
    assert!(!wrapper.contains("entry_symbol="));
    assert!(!wrapper.contains("link_mode_args="));
    assert!(!wrapper.contains("dynamic_platform="));
    assert!(!wrapper.contains("_head"));
}

#[test]
fn std_build_dynamic_x86_64_prepares_binary_artifact() {
    let metadata = repo_metadata();
    let cargo = BuildInfo {
        ..BuildInfo::default()
    }
    .into_prepared_base_cargo_config_with_metadata(
        "arceos-helloworld",
        "x86_64-unknown-none",
        &metadata,
    )
    .unwrap();

    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/x86_64-unknown-linux-musl.json")
    );
    assert!(!cargo.to_bin);
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(!cargo.features.contains(&"ax-std/smp".to_string()));
    assert_eq!(
        cargo.env.get("AX_TARGET"),
        Some(&"x86_64-unknown-none".to_string())
    );
}

use super::*;

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

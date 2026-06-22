use super::*;

#[test]
fn std_c_toolchain_env_does_not_require_installed_cross_compiler() {
    let env = std_c_toolchain_env("riscv64gc-unknown-linux-musl", "definitely-missing-musl");

    assert_eq!(
        env.get("CC_riscv64gc_unknown_linux_musl"),
        Some(&"definitely-missing-musl-cc".to_string())
    );
    assert_eq!(
        env.get("AR_riscv64gc_unknown_linux_musl"),
        Some(&"definitely-missing-musl-ar".to_string())
    );
    assert_eq!(
        env.get("CFLAGS_riscv64gc_unknown_linux_musl"),
        Some(&"-march=rv64gc -mabi=lp64d -mcmodel=medany".to_string())
    );
    assert_eq!(
        env.get("CXXFLAGS_riscv64gc_unknown_linux_musl"),
        Some(&"-march=rv64gc -mabi=lp64d -mcmodel=medany".to_string())
    );
    assert!(!env.contains_key("BINDGEN_EXTRA_CLANG_ARGS_riscv64gc_unknown_linux_musl"));
}

#[test]
fn std_c_toolchain_env_exports_loongarch_softfloat_abi_flags() {
    let env = std_c_toolchain_env("loongarch64-unknown-linux-musl", "loongarch64-linux-musl");

    assert_eq!(
        env.get("CFLAGS_loongarch64_unknown_linux_musl"),
        Some(&"-mabi=lp64s -msoft-float".to_string())
    );
    assert_eq!(
        env.get("CXXFLAGS_loongarch64_unknown_linux_musl"),
        Some(&"-mabi=lp64s -msoft-float".to_string())
    );
    if let Some(bindgen_args) = env.get("BINDGEN_EXTRA_CLANG_ARGS_loongarch64_unknown_linux_musl") {
        assert!(bindgen_args.contains("--target=loongarch64-linux-musl"));
        assert!(bindgen_args.contains("-mabi=lp64s"));
        assert!(bindgen_args.contains("-msoft-float"));
    }
}

#[test]
fn std_target_specs_keep_kernel_fields_with_std_identity() {
    for (std_target, plat_dyn, llvm_target, arch, pointer_width) in [
        (
            "x86_64-unknown-linux-musl",
            false,
            "x86_64-unknown-none-elf",
            "x86_64",
            64,
        ),
        (
            "x86_64-unknown-linux-musl",
            true,
            "x86_64-unknown-none-elf",
            "x86_64",
            64,
        ),
        (
            "aarch64-unknown-linux-musl",
            false,
            "aarch64-unknown-none",
            "aarch64",
            64,
        ),
        (
            "aarch64-unknown-linux-musl",
            true,
            "aarch64-unknown-none",
            "aarch64",
            64,
        ),
        (
            "riscv64gc-unknown-linux-musl",
            false,
            "riscv64",
            "riscv64",
            64,
        ),
        (
            "riscv64gc-unknown-linux-musl",
            true,
            "riscv64",
            "riscv64",
            64,
        ),
        (
            "loongarch64-unknown-linux-musl",
            false,
            "loongarch64-unknown-none",
            "loongarch64",
            64,
        ),
        (
            "loongarch64-unknown-linux-musl",
            true,
            "loongarch64-unknown-none",
            "loongarch64",
            64,
        ),
    ] {
        let workspace = crate::context::workspace_root_path().unwrap();
        let std_path = workspace.join(std_target_json_path(std_target, plat_dyn));
        assert!(
            std_path.exists(),
            "missing std target spec {}",
            std_path.display()
        );

        let std_spec: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&std_path).unwrap()).unwrap();

        assert_eq!(std_spec["arch"], arch);
        assert_eq!(std_spec["llvm-target"], llvm_target);
        assert_eq!(std_spec["target-pointer-width"], pointer_width);
        assert_eq!(std_spec["os"], "linux");
        assert_eq!(std_spec["env"], "musl");
        assert_eq!(std_spec["target-family"], serde_json::json!(["unix"]));
        assert_eq!(std_spec["has-thread-local"], true);
        let expected_tls_model = if std_target.starts_with("riscv64") {
            "local-exec"
        } else {
            "initial-exec"
        };
        assert_eq!(std_spec["tls-model"], expected_tls_model);
        assert_eq!(std_spec["metadata"]["std"], true);
        assert!(
            std_spec
                .pointer("/metadata/description")
                .and_then(|value| value.as_str())
                .is_some_and(|description| description.contains("musl identity"))
        );
        assert_eq!(std_spec["eh-frame-header"], false);
        assert_eq!(std_spec["relro-level"], "off");
        assert_eq!(std_spec["linker"], "rust-lld");
        assert_eq!(std_spec["linker-flavor"], "gnu-lld");
        assert_eq!(std_spec["panic-strategy"], "abort");
    }

    let loongarch = serde_json::from_str::<serde_json::Value>(
        &fs::read_to_string(crate::context::workspace_root_path().unwrap().join(
            std_target_json_path("loongarch64-unknown-linux-musl", false),
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(loongarch["llvm-abiname"], "lp64s");
    assert_eq!(loongarch["features"], "-f,-d");
}

#[test]
fn std_target_specs_do_not_import_linux_userspace_link_fields() {
    for (target, plat_dyn) in [
        ("x86_64-unknown-linux-musl", false),
        ("x86_64-unknown-linux-musl", true),
        ("aarch64-unknown-linux-musl", false),
        ("aarch64-unknown-linux-musl", true),
        ("riscv64gc-unknown-linux-musl", false),
        ("riscv64gc-unknown-linux-musl", true),
        ("loongarch64-unknown-linux-musl", false),
        ("loongarch64-unknown-linux-musl", true),
    ] {
        let path = crate::context::workspace_root_path()
            .unwrap()
            .join(std_target_json_path(target, plat_dyn));
        assert!(path.exists(), "missing std target spec {}", path.display());

        let spec: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(spec.get("dynamic-linking").is_none());
        assert!(spec.get("has-rpath").is_none());
        assert!(spec.get("pre-link-objects-fallback").is_none());
        assert!(spec.get("post-link-objects-fallback").is_none());
        assert!(spec.get("crt-static-default").is_none());
        assert!(spec.get("crt-static-respected").is_none());
        assert!(spec.get("supported-split-debuginfo").is_none());
        assert!(spec.get("supports-xray").is_none());
    }
}

#[test]
fn std_target_specs_embed_final_link_policy() {
    let cases = [
        ("x86_64-unknown-linux-musl", false, "_start", "-no-pie"),
        ("x86_64-unknown-linux-musl", true, "_head", "-pie"),
        ("aarch64-unknown-linux-musl", false, "_start", "-no-pie"),
        ("aarch64-unknown-linux-musl", true, "_head", "-pie"),
        ("riscv64gc-unknown-linux-musl", false, "_start", "-no-pie"),
        ("riscv64gc-unknown-linux-musl", true, "_head", "-pie"),
        ("loongarch64-unknown-linux-musl", false, "_start", "-no-pie"),
        ("loongarch64-unknown-linux-musl", true, "_head", "-pie"),
    ];

    for (target, plat_dyn, entry, mode_arg) in cases {
        let path = crate::context::workspace_root_path()
            .unwrap()
            .join(std_target_json_path(target, plat_dyn));
        let spec: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let link_args = gnu_lld_pre_link_args(&spec);

        assert!(link_args.contains(&mode_arg));
        assert!(link_args.contains(&"--gc-sections"));
        assert!(link_args.contains(&"-znorelro"));
        assert!(link_args.contains(&"-znostart-stop-gc"));
        assert!(link_args.contains(&"-Tlinker.x"));
        assert!(link_args.contains(&"-u"));
        assert!(link_args.contains(&entry));
        assert_eq!(spec["eh-frame-header"], false);
        assert_eq!(spec["relro-level"], "off");

        if plat_dyn {
            assert!(!link_args.contains(&"-static"));
            assert!(!link_args.contains(&"-no-pie"));
        } else {
            assert!(link_args.contains(&"-static"));
            assert!(!link_args.contains(&"-pie"));
        }
    }
}

use super::*;
use crate::build::info::{
    BuildInfo, package_enables_mmu, package_enables_mmu_with_defaults, package_mmu_enabled,
    toolchain_rustflags, toolchain_rustflags_for_mmu,
};

#[test]
fn duplicate_mmu_capabilities_add_only_one_stack_protector_rustflag() {
    let flags = toolchain_rustflags_for_mmu(&HashMap::new(), true);

    assert_eq!(
        flags
            .iter()
            .filter(|flag| flag.as_str() == "-Zstack-protector=strong")
            .count(),
        1
    );
}

#[test]
fn package_metadata_resolves_mmu_capability_through_defaults_and_aliases() {
    let metadata = repo_metadata();

    for package in ["arceos-helloworld", "starryos", "axvisor"] {
        assert!(
            package_enables_mmu(package, &metadata, &[]).unwrap(),
            "{package} must resolve a paging-enabled dependency"
        );
    }
    assert!(package_enables_mmu("ax-runtime", &metadata, &["paging".into()]).unwrap());
    assert!(package_enables_mmu("ax-libc", &metadata, &["paging".into()]).unwrap());
    assert!(package_enables_mmu("ax-std", &metadata, &["dns".into()]).unwrap());
    assert!(package_enables_mmu("ax-runtime", &metadata, &["vmap-task-stack".into()]).unwrap());
    assert!(!package_enables_mmu("ax-hal", &metadata, &["uspace".into()]).unwrap());
    assert!(!package_enables_mmu("ax-runtime", &metadata, &[]).unwrap());
    assert!(package_enables_mmu_with_defaults("arceos-helloworld", &metadata, &[], true,).unwrap());
    assert!(
        !package_enables_mmu_with_defaults("arceos-helloworld", &metadata, &[], false,).unwrap()
    );
}

#[test]
fn package_mmu_detection_does_not_mutate_feature_selection() {
    let metadata = repo_metadata();

    let libc = BuildInfo {
        features: vec!["fs".to_string()],
        ..BuildInfo::default()
    };
    assert!(package_mmu_enabled("ax-libc", &metadata, &libc.features, true).unwrap());
    assert_eq!(libc.features, vec!["fs".to_string()]);

    let app = BuildInfo::default();
    assert!(package_mmu_enabled("arceos-helloworld", &metadata, &app.features, true).unwrap());
    assert!(app.features.is_empty());

    assert!(!package_mmu_enabled("arceos-helloworld", &metadata, &app.features, false,).unwrap());
}

#[test]
fn package_mmu_rustflags_follow_metadata_without_injecting_features() {
    let metadata = repo_metadata();
    let cargo = BuildInfo::default()
        .into_prepared_no_std_cargo_config_with_metadata_and_defaults(
            "arceos-helloworld",
            "x86_64-unknown-none",
            &metadata,
            BareKernelLinkMode::Default,
            true,
        )
        .unwrap();
    assert!(cargo.features.is_empty());
    assert!(cargo.args.join("\n").contains("-Zstack-protector=strong"));

    let cargo = BuildInfo::default()
        .into_prepared_no_std_cargo_config_with_metadata_and_defaults(
            "arceos-helloworld",
            "x86_64-unknown-none",
            &metadata,
            BareKernelLinkMode::Default,
            false,
        )
        .unwrap();
    assert!(!cargo.args.join("\n").contains("-Zstack-protector=strong"));
}

#[test]
fn axlibc_paging_alias_is_normalized_before_cargo_feature_resolution() {
    let metadata = repo_metadata();
    let app = BuildInfo {
        features: vec!["axlibc/paging".to_string()],
        ..BuildInfo::default()
    };

    let cargo = app
        .into_prepared_no_std_cargo_config_with_metadata(
            "arceos-helloworld",
            "x86_64-unknown-none",
            &metadata,
            BareKernelLinkMode::Default,
        )
        .unwrap();

    assert_eq!(cargo.features, vec!["ax-std/paging".to_string()]);
}

#[test]
fn toolchain_rustflags_preserves_debug_and_backtrace_env() {
    let env = HashMap::from([("DWARF".to_string(), "1".to_string())]);

    assert_eq!(
        toolchain_rustflags(&env),
        vec![
            "-Cdebuginfo=2".to_string(),
            "-Cstrip=none".to_string(),
            "-Cforce-frame-pointers=yes".to_string(),
        ]
    );
}

#[test]
fn appended_rustflags_preserve_quoted_inline_target_contract() {
    let mut cargo = Cargo {
        target: "x86_64-unknown-none".into(),
        args: vec![
            "--config".into(),
            concat!(
                "target.'x86_64-unknown-none'.rustflags=[",
                "\"-Crelocation-model=pic\", ",
                "\"-Clink-args=-Tlinker.x\"",
                "]"
            )
            .into(),
        ],
        ..Cargo::default()
    };

    append_cargo_rustflags(&mut cargo, &["-Cdebuginfo=2"]);

    let rendered = cargo.args.join("\n");
    assert!(rendered.contains("-Clink-args=-Tlinker.x"));
    assert!(rendered.contains("-Cdebuginfo=2"));
    assert!(
        !cargo.env.contains_key("CARGO_ENCODED_RUSTFLAGS"),
        "encoded rustflags would shadow the quoted target linker contract"
    );
}

#[test]
fn appended_rustflags_preserve_plain_rustflags_source() {
    let mut cargo = Cargo {
        target: "x86_64-unknown-none".into(),
        env: [("RUSTFLAGS".into(), "-Cdebuginfo=1 -Cstrip=none".into())].into(),
        ..Cargo::default()
    };

    append_cargo_rustflags(&mut cargo, &["-Cforce-frame-pointers=yes"]);

    assert_eq!(
        cargo.env.get("CARGO_ENCODED_RUSTFLAGS").map(String::as_str),
        Some("-Cdebuginfo=1\x1f-Cstrip=none\x1f-Cforce-frame-pointers=yes")
    );
    assert!(!cargo.env.contains_key("RUSTFLAGS"));
}

#[test]
fn appended_build_rustflags_preserve_cargo_build_env_source() {
    let mut cargo = Cargo {
        target: "x86_64-unknown-none".into(),
        env: [("CARGO_BUILD_RUSTFLAGS".into(), "-Cdebuginfo=1".into())].into(),
        ..Cargo::default()
    };

    append_cargo_rustflags(&mut cargo, &["-Cforce-frame-pointers=yes"]);

    assert_eq!(
        cargo.env.get("CARGO_BUILD_RUSTFLAGS").map(String::as_str),
        Some("-Cdebuginfo=1 -Cforce-frame-pointers=yes")
    );
    assert!(
        !cargo.args.join("\n").contains("target."),
        "a target rustflags source would shadow build.rustflags"
    );
}

#[test]
fn appended_rustflags_preserve_target_environment_source() {
    let mut cargo = Cargo {
        target: "x86_64-unknown-none".into(),
        env: [(
            "CARGO_TARGET_X86_64_UNKNOWN_NONE_RUSTFLAGS".into(),
            "-Crelocation-model=pic".into(),
        )]
        .into(),
        ..Cargo::default()
    };

    append_cargo_rustflags(&mut cargo, &["-Cdebuginfo=2"]);

    assert_eq!(
        cargo
            .env
            .get("CARGO_TARGET_X86_64_UNKNOWN_NONE_RUSTFLAGS")
            .map(String::as_str),
        Some("-Crelocation-model=pic -Cdebuginfo=2")
    );
    assert!(cargo.args.is_empty());
}

#[test]
fn appended_rustflags_keep_space_containing_arguments_intact() {
    let mut cargo = Cargo {
        target: "x86_64-unknown-none".into(),
        env: [(
            "CARGO_TARGET_X86_64_UNKNOWN_NONE_RUSTFLAGS".into(),
            "-Crelocation-model=pic".into(),
        )]
        .into(),
        ..Cargo::default()
    };

    append_cargo_rustflags(&mut cargo, &["-Clink-args=-u _head"]);

    assert_eq!(
        cargo
            .env
            .get("CARGO_TARGET_X86_64_UNKNOWN_NONE_RUSTFLAGS")
            .map(String::as_str),
        Some("-Crelocation-model=pic")
    );
    assert!(
        cargo.args.contains(
            &concat!(
                "target.\"x86_64-unknown-none\".rustflags=[",
                "\"-Clink-args=-u _head\"",
                "]"
            )
            .to_string()
        )
    );
}

#[test]
fn appended_rustflags_deduplicate_only_the_complete_sequence() {
    let mut cargo = Cargo {
        target: "x86_64-unknown-none".into(),
        env: [("CARGO_ENCODED_RUSTFLAGS".into(), "--cfg\x1fother".into())].into(),
        ..Cargo::default()
    };

    append_cargo_rustflags(&mut cargo, &["--cfg", "axtest"]);
    append_cargo_rustflags(&mut cargo, &["--cfg", "axtest"]);

    assert_eq!(
        cargo.env.get("CARGO_ENCODED_RUSTFLAGS").map(String::as_str),
        Some("--cfg\x1fother\x1f--cfg\x1faxtest")
    );
}

#[test]
fn build_info_rejects_uspace_and_tls_register_modes_before_cargo() {
    for features in [
        vec!["uspace".to_string(), "tls".to_string()],
        vec!["ax-std/uspace".to_string(), "ax-std/tls".to_string()],
    ] {
        let info = BuildInfo {
            features,
            ..BuildInfo::default()
        };

        let error = info.validate_features().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("incompatible CPU-local register")
        );
    }
}

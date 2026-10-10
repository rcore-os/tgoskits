use super::*;
use crate::build::info::{
    BuildInfo, ensure_package_mmu_feature, features_enable_mmu, package_enables_mmu,
    toolchain_rustflags, toolchain_rustflags_for_features,
};

#[test]
fn paging_capability_enables_stack_protector_rustflag() {
    let env = HashMap::new();
    for feature in [
        "paging",
        "ax-std/paging",
        "ax-runtime/paging",
        "ax-hal/paging",
        "ax-std/uspace",
        "ax-runtime/uspace",
        "ax-hal/uspace",
        "ax-std/hv",
        "ax-hal/hv",
        "ax-libc/paging",
        "axlibc/paging",
    ] {
        assert!(features_enable_mmu(&[feature.to_string()]), "{feature}");
        assert_eq!(
            toolchain_rustflags_for_features(&env, &[feature.to_string()])
                .iter()
                .filter(|flag| flag.as_str() == "-Zstack-protector=strong")
                .count(),
            1,
            "{feature} must add one compiler stack-protector flag"
        );
    }
}

#[test]
fn non_paging_build_does_not_enable_stack_protector_rustflag() {
    let features = ["smp".to_string()];
    let flags = toolchain_rustflags_for_features(&HashMap::new(), &features);

    assert!(!features_enable_mmu(&features));
    assert!(!flags.iter().any(|flag| flag == "-Zstack-protector=strong"));
}

#[test]
fn duplicate_mmu_capabilities_add_only_one_stack_protector_rustflag() {
    let features = vec![
        "paging".to_string(),
        "ax-std/paging".to_string(),
        "ax-runtime/paging".to_string(),
    ];
    let flags = toolchain_rustflags_for_features(&HashMap::new(), &features);

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
    assert!(!package_enables_mmu("ax-runtime", &metadata, &[]).unwrap());
}

#[test]
fn package_mmu_injection_uses_a_feature_declared_by_the_target_package() {
    let metadata = repo_metadata();

    let mut libc = BuildInfo {
        features: vec!["fs".to_string()],
        ..BuildInfo::default()
    };
    ensure_package_mmu_feature(&mut libc, "ax-libc", &metadata).unwrap();
    assert_eq!(libc.features, vec!["fs".to_string(), "paging".to_string()]);

    let mut app = BuildInfo::default();
    ensure_package_mmu_feature(&mut app, "arceos-helloworld", &metadata).unwrap();
    assert_eq!(app.features, vec!["ax-std/paging".to_string()]);
}

#[test]
fn axlibc_paging_alias_is_normalized_before_cargo_feature_resolution() {
    let metadata = repo_metadata();
    let mut app = BuildInfo {
        features: vec!["axlibc/paging".to_string()],
        ..BuildInfo::default()
    };

    ensure_package_mmu_feature(&mut app, "arceos-helloworld", &metadata).unwrap();

    assert_eq!(app.features, vec!["ax-std/paging".to_string()]);
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

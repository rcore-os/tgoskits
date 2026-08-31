use super::*;
use crate::build::info::{
    build_info_enables_backtrace, features_enable_stack_protector, toolchain_rustflags,
};

#[test]
fn build_info_enables_backtrace_matches_env_flags() {
    let mut info = BuildInfo::default();
    assert!(!build_info_enables_backtrace(&info));

    info.env.insert("BACKTRACE".to_string(), "y".to_string());
    assert!(build_info_enables_backtrace(&info));

    info.env.clear();
    info.env.insert("DWARF".to_string(), "1".to_string());
    assert!(build_info_enables_backtrace(&info));
}

#[test]
fn build_info_defaults_to_empty_env() {
    let info = BuildInfo::default();
    assert!(info.env.is_empty());
    assert!(info.features.is_empty());
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
fn toolchain_rustflags_enable_stack_protector_from_features() {
    let env = HashMap::from([("BACKTRACE".to_string(), "y".to_string())]);
    let features = vec!["ax-std/stack-protector".to_string()];

    assert_eq!(
        toolchain_rustflags_for_features(&env, &features),
        vec![
            "-Cforce-frame-pointers=yes".to_string(),
            "-Zstack-protector=strong".to_string(),
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
fn appended_rustflags_accept_joined_cargo_config_form() {
    let mut cargo = Cargo {
        target: "x86_64-unknown-none".into(),
        args: vec![
            concat!(
                "--config=target.x86_64-unknown-none.rustflags=[",
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
    assert!(!cargo.env.contains_key("CARGO_ENCODED_RUSTFLAGS"));
}

#[test]
fn appended_rustflags_preserve_string_target_config_form() {
    let mut cargo = Cargo {
        target: "x86_64-unknown-none".into(),
        args: vec![
            "--config".into(),
            "target.x86_64-unknown-none.rustflags=\"-Clink-args=-Tlinker.x\"".into(),
        ],
        ..Cargo::default()
    };

    append_cargo_rustflags(&mut cargo, &["-Cdebuginfo=2"]);

    let rendered = cargo.args.join("\n");
    assert!(rendered.contains("-Clink-args=-Tlinker.x"));
    assert!(rendered.contains("-Cdebuginfo=2"));
    assert!(!cargo.env.contains_key("CARGO_ENCODED_RUSTFLAGS"));
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
fn appended_rustflags_stay_in_target_config_with_extra_config() {
    let mut cargo = Cargo {
        target: "x86_64-unknown-linux-musl".into(),
        extra_config: Some("target/axbuild-std/x86_64/config.toml".into()),
        ..Cargo::default()
    };

    append_cargo_rustflags(&mut cargo, &["--cfg", "axtest"]);

    assert!(!cargo.env.contains_key("CARGO_ENCODED_RUSTFLAGS"));
    assert!(cargo.args.join("\n").contains("--cfg"));
    assert!(cargo.args.join("\n").contains("axtest"));
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
fn appended_build_rustflags_preserve_inline_build_config_source() {
    let mut cargo = Cargo {
        target: "x86_64-unknown-none".into(),
        args: vec![
            "--config".into(),
            "build.rustflags=[\"-Cdebuginfo=1\"]".into(),
        ],
        ..Cargo::default()
    };

    append_cargo_rustflags(&mut cargo, &["-Cforce-frame-pointers=yes"]);

    let rendered = cargo.args.join("\n");
    assert!(rendered.contains("-Cdebuginfo=1"));
    assert!(rendered.contains("-Cforce-frame-pointers=yes"));
    assert!(
        !rendered.contains("target."),
        "a target rustflags source would shadow build.rustflags"
    );
}

#[test]
fn appended_build_rustflags_preserve_extra_build_config_source() {
    let temp = tempdir().unwrap();
    let config = temp.path().join("config.toml");
    fs::write(&config, "[build]\nrustflags = [\"-Cdebuginfo=1\"]\n").unwrap();
    let mut cargo = Cargo {
        target: "x86_64-unknown-none".into(),
        extra_config: Some(config.display().to_string()),
        ..Cargo::default()
    };

    append_cargo_rustflags(&mut cargo, &["-Cforce-frame-pointers=yes"]);

    let rendered = cargo.args.join("\n");
    assert!(rendered.contains("build.rustflags"));
    assert!(rendered.contains("-Cforce-frame-pointers=yes"));
    assert!(
        !rendered.contains("target."),
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
fn stack_protector_feature_detection_accepts_supported_surfaces() {
    for feature in [
        "stack-protector",
        "ax-std/stack-protector",
        "starry-kernel/stack-protector",
    ] {
        assert!(features_enable_stack_protector(&[feature.to_string()]));
    }

    assert!(!features_enable_stack_protector(&[
        "stack-guard-page".to_string()
    ]));
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

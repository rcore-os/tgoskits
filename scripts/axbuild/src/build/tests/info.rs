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
fn build_info_accepts_missing_env() {
    let info: BuildInfo = toml::from_str(
        r#"
features = []
log = "Info"
"#,
    )
    .unwrap();

    assert!(info.env.is_empty());
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

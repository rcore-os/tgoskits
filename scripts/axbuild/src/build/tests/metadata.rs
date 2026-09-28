use super::*;

#[test]
fn rejects_legacy_and_removed_platform_features() {
    for feature in ["axstd", "axstd/net", "plat-dyn", "ax-std/plat-dyn"] {
        let info = BuildInfo {
            features: vec![feature.to_string()],
            ..BuildInfo::default()
        };

        assert!(
            info.validate_features().is_err(),
            "{feature} must be rejected"
        );
    }
}

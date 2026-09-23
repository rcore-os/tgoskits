use super::*;

#[test]
fn std_build_nested_features_are_passed_through_not_enabled_on_app() {
    let mut features = vec![
        "ax-driver/nvme".to_string(),
        "ax-driver/virtio-net".to_string(),
        "dns".to_string(),
    ];

    pass_std_build_nested_features(
        &mut features,
        &["dns".to_string()],
        &[
            "dns".to_string(),
            "plat-dyn".to_string(),
            "std-compat".to_string(),
            "nvme".to_string(),
            "virtio-net".to_string(),
        ],
    );

    assert!(features.contains(&"ax-std/dns".to_string()));
    assert!(features.contains(&"ax-std/nvme".to_string()));
    assert!(features.contains(&"ax-std/virtio-net".to_string()));
    assert!(features.contains(&"dns".to_string()));
}

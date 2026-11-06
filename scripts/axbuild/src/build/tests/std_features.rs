use super::*;

#[test]
fn std_build_nested_features_are_passed_through_not_enabled_on_app() {
    let mut info = CommonConfig {
        features: vec![
            "ax-driver/nvme".into(),
            "ax-driver/virtio-net".into(),
            "dns".into(),
        ],
        max_cpu_num: Some(4),
        ..Default::default()
    };
    let app_features = ["dns".to_string()];
    let axstd_features = [
        "dns".into(),
        "nvme".into(),
        "virtio-net".into(),
        "smp".into(),
    ];
    let features = resolved_std_build_features(&info, &app_features, &axstd_features)
        .unwrap();
    assert!(features.contains(&"ax-std/smp".to_string()));
    info.max_cpu_num = Some(1);
    assert!(
        !resolved_std_build_features(&info, &app_features, &axstd_features)
            .unwrap()
            .contains(&"ax-std/smp".to_string())
    );
    info.max_cpu_num = Some(0);
    assert!(
        resolved_std_build_features(&info, &app_features, &axstd_features)
            .is_err()
    );

    assert!(features.contains(&"ax-std/dns".to_string()));
    assert!(features.contains(&"ax-std/nvme".to_string()));
    assert!(features.contains(&"ax-std/virtio-net".to_string()));
    assert!(features.contains(&"dns".to_string()));
}

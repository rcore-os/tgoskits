use crate::test::qemu::{summary::*, tree::*, types::*};

#[test]
fn qemu_failure_summary_is_aggregated() {
    let mut summary = QemuTestSummary::default();
    summary.pass_with_detail("pkg-a", "0.10s");
    summary.fail_with_detail("pkg-b", "0.20s");
    summary.fail_with_detail("pkg-c", "0.30s");

    let err = summary
        .finish_with_total_detail("arceos", "package", Some("0.60s"))
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("arceos qemu tests failed for 2 package(s): pkg-b, pkg-c")
    );
}

#[test]
fn render_case_tree_uses_group_root() {
    assert_eq!(
        render_case_tree("normal", ["qemu/apk-curl", "qemu/smoke", "qemu/system",],),
        "normal\n└── qemu\n    ├── apk-curl\n    ├── smoke\n    └── system"
    );
}

#[test]
fn render_qemu_case_forest_appends_arch_labels_to_leaves() {
    assert_eq!(
        render_qemu_case_forest(
            "arceos",
            [(
                "rust",
                vec![
                    ListedQemuCase {
                        name: "task/yield".to_string(),
                        archs: vec!["aarch64".to_string(), "x86_64".to_string()],
                    },
                    ListedQemuCase {
                        name: "display".to_string(),
                        archs: vec!["x86_64".to_string()],
                    },
                ],
            )],
        ),
        "arceos\n└── rust\n    ├── display [x86_64]\n    └── task\n        └── yield [aarch64, \
         x86_64]"
    );
}

#[test]
fn render_labeled_case_forest_appends_board_labels_to_leaves() {
    assert_eq!(
        render_labeled_case_forest(
            "starry",
            [(
                "normal",
                vec![("smoke", "orangepi-5-plus"), ("smoke", "vision-five2"),],
            )],
        ),
        "starry\n└── normal\n    └── smoke [orangepi-5-plus, vision-five2]"
    );
}

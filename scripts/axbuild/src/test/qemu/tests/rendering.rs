use crate::test::qemu::summary::*;

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

use ostool::run::qemu::QemuConfig;

use super::super::{
    ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE, ARCEOS_RUST_LOCKDEP_DETECT_FEATURE,
    ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE,
};

pub(crate) fn apply_rust_qemu_feature_overrides(qemu: &mut QemuConfig, feature: Option<&str>) {
    match feature {
        Some(ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE) => {
            crate::support::qemu_success::replace_configured_success_regex(qemu, vec![
                r"(?s)ARCEOS_PANIC_EMERGENCY(?-u:\b).*backtrace panic-path smoke test.*(?-u:\b)BACKTRACE_BEGIN(?-u:\b).*(?-u:\b)kind=panic(?-u:\b).*symbol=[^\s]+ at .+:[0-9]+"
                    .to_string(),
            ]);
            qemu.fail_regex = vec!["ARCEOS_TEST_FAIL".to_string()];
            qemu.timeout = Some(qemu.timeout.unwrap_or(30).min(30));
        }
        Some(feature) if is_lockdep_detect_feature(feature) => {
            crate::support::qemu_success::replace_configured_success_regex(
                qemu,
                vec!["lockdep: lock order inversion detected".to_string()],
            );
            qemu.fail_regex =
                vec![r"lockdep did not report an expected .*lock order inversion".to_string()];
            qemu.timeout = Some(qemu.timeout.unwrap_or(30).min(30));
        }
        Some(ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE) => {
            crate::support::qemu_success::replace_configured_success_regex(qemu, vec![r"(?s)ARCEOS_TEST_BEGIN feature=task-stack-guard-page(?-u:\b).*task stack guard page hit: fault_addr=0x[0-9a-f]+, stack=\[0x[0-9a-f]+\.\.0x[0-9a-f]+\), guard=\[0x[0-9a-f]+\.\.0x[0-9a-f]+\)".to_string()]);
            qemu.fail_regex = vec!["stack guard page was not hit".to_string()];
            qemu.timeout = Some(qemu.timeout.unwrap_or(30).min(30));
        }
        _ => {}
    }
}

fn is_lockdep_detect_feature(feature: &str) -> bool {
    matches!(feature, ARCEOS_RUST_LOCKDEP_DETECT_FEATURE)
}

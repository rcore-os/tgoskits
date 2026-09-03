//! CPU-time transitions have both execution-owner and scheduler-policy writers.

const ACCOUNTING: &str = include_str!("../src/task/timer/accounting.rs");
const SCHEDULER_TASK: &str = include_str!("../src/task/scheduler_task.rs");

#[test]
fn running_policy_updates_share_the_cpu_time_sequence_writer() {
    assert!(SCHEDULER_TASK.contains(".with_running_policy_applied_hook("));
    assert!(SCHEDULER_TASK.contains(".apply_cpu_time_policy("));

    let accounting = struct_body(ACCOUNTING, "pub struct CpuTimeAccounting");
    assert!(
        accounting.contains("sequence: AtomicU64"),
        "CPU-time readers and writers must share one sequence word"
    );

    let begin_write = function_body(ACCOUNTING, "fn begin_write(");
    assert!(begin_write.contains("NoPreemptIrqSave::new()"));
    assert!(begin_write.contains("compare_exchange_weak("));
    let finish_write = function_body(ACCOUNTING, "fn drop(");
    assert!(finish_write.contains("Ordering::Release"));
    assert!(!ACCOUNTING.contains("CPU-time accounting owner was re-entered"));
}

fn struct_body<'a>(source: &'a str, signature: &str) -> &'a str {
    braced_body(source, signature)
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    braced_body(source, signature)
}

fn braced_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing source signature: {signature}"));
    let body_start = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("missing body for source signature: {signature}"));
    let mut depth = 0usize;
    for (offset, byte) in source.as_bytes()[body_start..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[body_start + 1..body_start + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated body for source signature: {signature}");
}

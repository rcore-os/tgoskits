use axbacktrace::Backtrace;

#[test]
fn report_without_init_emits_not_initialized_error() {
    let output = format!("{}", Backtrace::report("panic"));
    assert!(output.contains("BACKTRACE_BEGIN kind=panic"));
    assert!(output.contains("BT_ERROR not_initialized"));
    assert!(output.contains("BACKTRACE_END"));
}

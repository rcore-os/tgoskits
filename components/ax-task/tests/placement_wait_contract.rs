const PLACEMENT: &str = include_str!("../src/system/thread_sched/placement.rs");

#[test]
fn on_cpu_wait_uses_linux_conditional_acquire_polling() {
    let wait = PLACEMENT
        .split_once("pub(in crate::system) fn wait_until_not_on_cpu")
        .expect("on_cpu wait helper must remain present")
        .1
        .split_once("\n    pub(in crate::system) fn committed_migration_target")
        .expect("on_cpu wait helper must remain focused")
        .0;

    let relaxed = wait
        .find("self.on_cpu.load(Ordering::Relaxed)")
        .expect("spin polling should use a relaxed on_cpu load");
    assert!(
        wait[relaxed..].contains("self.on_cpu.load(Ordering::Acquire)"),
        "the relaxed poll must be followed by a final acquire load"
    );
    assert!(
        !wait.contains("while self.on_cpu().is_some()"),
        "the hot spin loop must not decode the CPU on every iteration"
    );
}

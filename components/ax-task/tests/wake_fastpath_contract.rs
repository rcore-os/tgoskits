const REMOTE_SCHEDULER: &str = include_str!("../src/system/cpu/remote/scheduler.rs");

#[test]
fn rq_wake_retries_delivery_while_the_cpu_request_is_sticky() {
    let rq_publication = REMOTE_SCHEDULER
        .split_once("pub(crate) fn publish_rq_scheduler_reasons(")
        .expect("rq scheduler-reason publication must remain present")
        .1
        .split_once("\n    /// Publishes a remote preemption")
        .expect("rq scheduler-reason publication must remain focused")
        .0;
    assert!(
        rq_publication.contains(".publish_rq_delivery(reasons)"),
        "wake-under-rq must use the retriable delivery publication"
    );

    let delivery = REMOTE_SCHEDULER
        .split_once("fn publish_rq_delivery(&self, reason: u64) -> Option<u64> {")
        .expect("rq delivery helper must remain present")
        .1
        .split_once("\n    }")
        .expect("rq delivery helper must remain focused")
        .0;
    assert!(
        delivery.contains("self.publish_remote(reason)"),
        "a repeated immediate rq decision must retry the local preempt word or physical doorbell"
    );
}

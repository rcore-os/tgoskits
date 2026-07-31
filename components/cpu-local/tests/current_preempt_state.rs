use cpu_local::{CurrentContext, CurrentPreemptExit, CurrentThreadHeader};

fn task_header() -> CurrentThreadHeader {
    CurrentThreadHeader::new(
        CurrentContext::from_raw(1).expect("test context identity must be non-zero"),
    )
}

#[test]
fn nested_preempt_exit_consumes_only_nested_depth() {
    let header = task_header();
    assert_eq!(header.preempt_guard_depth(), 0);

    header.enter_preempt_guard();
    header.enter_preempt_guard();

    assert_eq!(
        header.prepare_preempt_guard_exit(),
        CurrentPreemptExit::NestedConsumed
    );
    assert_eq!(header.preempt_guard_depth(), 1);
}

#[test]
fn final_preempt_exit_without_reschedule_is_consumed_on_the_fast_path() {
    let header = task_header();
    header.enter_preempt_guard();

    assert_eq!(
        header.prepare_preempt_guard_exit(),
        CurrentPreemptExit::FinalConsumed
    );
    assert_eq!(header.preempt_guard_depth(), 0);
}

#[test]
fn final_preempt_exit_with_reschedule_stays_published_until_scheduler_claim() {
    let header = task_header();
    header.enter_preempt_guard();
    header.set_preempt_need_resched();

    assert_eq!(
        header.prepare_preempt_guard_exit(),
        CurrentPreemptExit::FinalPending
    );
    assert_eq!(
        header.preempt_guard_depth(),
        1,
        "the final depth must close the preemptible window before baton claim"
    );
    assert!(header.consume_final_preempt_guard());
    assert_eq!(header.preempt_guard_depth(), 0);
    assert!(header.preempt_need_resched());
    header.clear_preempt_need_resched();
    assert!(!header.preempt_need_resched());
    assert!(
        !header.consume_final_preempt_guard(),
        "one final guard generation can be consumed only once"
    );
}

#[test]
#[should_panic(expected = "unbalanced current-thread preemption guard exit")]
fn unbalanced_preempt_exit_is_rejected() {
    let header = task_header();
    let _ = header.prepare_preempt_guard_exit();
}

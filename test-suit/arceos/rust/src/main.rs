#[cfg(feature = "ax-std")]
use std::{println, time::Instant};

#[cfg(feature = "ax-std")]
use arceos_test_suit::selected_tests;
#[cfg(feature = "ax-std")]
use ax_std as _;
#[cfg(feature = "task-runtime")]
use {
    ax_std::os::arceos::task::sched::CpuSet, ax_std::os::arceos::task::sched::SchedulePolicy,
    ax_std::os::arceos::task::thread::ThreadId,
    ax_std::os::arceos::task::thread::current::current_thread_id,
    ax_std::os::arceos::task::thread::current::set_current_thread_affinity,
};

#[cfg(feature = "task-runtime")]
struct RunnerTaskState {
    thread: ThreadId,
    affinity: CpuSet,
    policy: SchedulePolicy,
}

#[cfg(feature = "task-runtime")]
impl RunnerTaskState {
    fn capture() -> Self {
        let thread = current_thread_id().expect("test runner must have a task identity");
        let affinity = ax_std::os::arceos::task::thread::ThreadHandle::lookup(thread)
            .and_then(|thread| thread.affinity())
            .expect("test runner must have CPU affinity");
        let policy = ax_std::os::arceos::task::thread::ThreadHandle::lookup(thread)
            .map(|thread| thread.base_policy())
            .expect("test runner must have a scheduling policy");
        Self {
            thread,
            affinity,
            policy,
        }
    }

    fn restore(self) {
        assert_eq!(
            current_thread_id(),
            Ok(self.thread),
            "an ArceOS test must not replace the shared runner task"
        );
        if ax_std::os::arceos::task::thread::ThreadHandle::lookup(self.thread)
            .map(|thread| thread.base_policy())
            != Ok(self.policy)
        {
            ax_std::os::arceos::task::thread::ThreadHandle::lookup(self.thread)
                .and_then(|thread| thread.set_policy(self.policy))
                .expect("failed to restore the test runner scheduling policy");
        }
        if ax_std::os::arceos::task::thread::ThreadHandle::lookup(self.thread)
            .and_then(|thread| thread.affinity())
            != Ok(self.affinity.clone())
        {
            set_current_thread_affinity(self.affinity.clone())
                .expect("failed to restore the test runner CPU affinity");
        }
        assert_eq!(
            ax_std::os::arceos::task::thread::ThreadHandle::lookup(self.thread)
                .and_then(|thread| thread.affinity()),
            Ok(self.affinity.clone()),
            "an ArceOS test must not leak runner CPU affinity"
        );
        assert_eq!(
            ax_std::os::arceos::task::thread::ThreadHandle::lookup(self.thread)
                .map(|thread| thread.base_policy()),
            Ok(self.policy),
            "an ArceOS test must not leak runner scheduling policy"
        );
    }
}

#[cfg(feature = "ax-std")]
fn main() {
    let tests = selected_tests();
    assert!(!tests.is_empty(), "no ArceOS test suite feature selected");

    println!("ArceOS test suite run begin: {} tests", tests.len());
    for test in tests {
        let started = Instant::now();
        println!(
            "ARCEOS_TEST_BEGIN feature={} name={}",
            test.feature, test.name
        );
        #[cfg(feature = "task-runtime")]
        let runner_state = RunnerTaskState::capture();
        let result = (test.run)();
        #[cfg(feature = "task-runtime")]
        runner_state.restore();
        match result {
            Ok(()) => {
                println!(
                    "ARCEOS_TEST_END feature={} name={} status=pass elapsed_ms={}",
                    test.feature,
                    test.name,
                    started.elapsed().as_millis()
                );
            }
            Err(message) => {
                println!(
                    "ARCEOS_TEST_END feature={} name={} status=fail elapsed_ms={} reason={}",
                    test.feature,
                    test.name,
                    started.elapsed().as_millis(),
                    message
                );
                panic!(
                    "ARCEOS_TEST_FAIL feature={} reason={}",
                    test.feature, message
                );
            }
        }
    }
    println!("ArceOS test suite run OK!");
}

#[cfg(not(feature = "ax-std"))]
fn main() {
    eprintln!("arceos-test-suit requires an ArceOS feature such as `all` for kernel runs");
}

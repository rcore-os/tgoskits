#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use std::{println, time::Instant};

#[cfg(feature = "ax-std")]
use arceos_test_suit::selected_tests;
#[cfg(feature = "task-runtime")]
use {
    std::os::arceos::task::sched::CpuSet, std::os::arceos::task::sched::SchedulePolicy,
    std::os::arceos::task::thread::ThreadId,
    std::os::arceos::task::thread::current::current_thread_id,
    std::os::arceos::task::thread::current::set_current_thread_affinity,
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
        let affinity = std::os::arceos::task::thread::ThreadHandle::lookup(thread)
            .and_then(|thread| thread.affinity())
            .expect("test runner must have CPU affinity");
        let policy = std::os::arceos::task::thread::ThreadHandle::lookup(thread)
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
        if std::os::arceos::task::thread::ThreadHandle::lookup(self.thread)
            .map(|thread| thread.base_policy())
            != Ok(self.policy)
        {
            std::os::arceos::task::thread::ThreadHandle::lookup(self.thread)
                .and_then(|thread| thread.set_policy(self.policy))
                .expect("failed to restore the test runner scheduling policy");
        }
        if std::os::arceos::task::thread::ThreadHandle::lookup(self.thread)
            .and_then(|thread| thread.affinity())
            != Ok(self.affinity.clone())
        {
            set_current_thread_affinity(self.affinity.clone())
                .expect("failed to restore the test runner CPU affinity");
        }
        assert_eq!(
            std::os::arceos::task::thread::ThreadHandle::lookup(self.thread)
                .and_then(|thread| thread.affinity()),
            Ok(self.affinity.clone()),
            "an ArceOS test must not leak runner CPU affinity"
        );
        assert_eq!(
            std::os::arceos::task::thread::ThreadHandle::lookup(self.thread)
                .map(|thread| thread.base_policy()),
            Ok(self.policy),
            "an ArceOS test must not leak runner scheduling policy"
        );
    }
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
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

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}

// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Axvisor realtime CPU partitioning and secondary CPU entry.

use core::sync::atomic::{AtomicU64, Ordering};

use ax_rt::{
    RtMutex, RtSemaphore, RtTask, rt_delay_until, rt_exit_current_task, rt_output_write, rt_sleep,
};
pub use ax_rt::{RtState, RtTaskState, rt_read_output, status};

const HEARTBEAT_INTERVAL_NANOS: u64 = 1_000_000;
const WATCHDOG_INTERVAL_NANOS: u64 = 100_000_000;
const HELLO_INTERVAL_NANOS: u64 = 1_000_000_000;
const HELLO_RUNS: u64 = 5;
const PRIORITY_TEST_LOW_READY: u64 = 1;
const PRIORITY_TEST_HIGH_BLOCKED: u64 = 2;
const PRIORITY_TEST_LOW_RELEASED: u64 = 3;
const PRIORITY_TEST_MEDIUM_RAN: u64 = 4;
const PRIORITY_TEST_HIGH_ACQUIRED: u64 = 5;
const RECURSIVE_TEST_OWNER_READY: u64 = 1;
const RECURSIVE_TEST_WAITER_BLOCKING: u64 = 2;
const RECURSIVE_TEST_INNER_DROPPED: u64 = 3;
const RECURSIVE_TEST_WAITER_ACQUIRED: u64 = 4;
const SEMAPHORE_TEST_WAITER_BLOCKING: u64 = 1;
const SEMAPHORE_TEST_WAITER_ACQUIRED: u64 = 2;

static RT_HEARTBEATS: AtomicU64 = AtomicU64::new(0);
static RT_WATCHDOG_RUNS: AtomicU64 = AtomicU64::new(0);
static RT_LAST_HEARTBEAT_NANOS: AtomicU64 = AtomicU64::new(0);
static RT_LAST_WATCHDOG_NANOS: AtomicU64 = AtomicU64::new(0);
static RT_SAMPLE_MUTEX: RtMutex = RtMutex::new();
static RT_PRIORITY_TEST_MUTEX: RtMutex = RtMutex::new();
static RT_PRIORITY_TEST_STEP: AtomicU64 = AtomicU64::new(0);
static RT_PRIORITY_TEST_RESULT: AtomicU64 = AtomicU64::new(0);
static RT_RECURSIVE_TEST_MUTEX: RtMutex = RtMutex::new();
static RT_RECURSIVE_TEST_STEP: AtomicU64 = AtomicU64::new(0);
static RT_RECURSIVE_TEST_RESULT: AtomicU64 = AtomicU64::new(0);
static RT_SEMAPHORE_TEST_SEM: RtSemaphore = RtSemaphore::new(0);
static RT_SEMAPHORE_TEST_STEP: AtomicU64 = AtomicU64::new(0);
static RT_SEMAPHORE_TEST_RESULT: AtomicU64 = AtomicU64::new(0);
static RT_TASKS: [RtTask; 10] = [
    RtTask::with_priority("heartbeat", HEARTBEAT_INTERVAL_NANOS, 10, heartbeat_task),
    RtTask::with_priority("watchdog", WATCHDOG_INTERVAL_NANOS, 5, watchdog_task),
    RtTask::with_priority("hello", HELLO_INTERVAL_NANOS, 1, hello_task),
    RtTask::with_priority("prio-low", 0, 20, priority_test_low_task),
    RtTask::with_priority("prio-high", 0, 40, priority_test_high_task),
    RtTask::with_priority("prio-mid", 0, 30, priority_test_medium_task),
    RtTask::with_priority("recur-own", 0, 25, recursive_test_owner_task),
    RtTask::with_priority("recur-wait", 0, 15, recursive_test_waiter_task),
    RtTask::with_priority("sem-wait", 0, 35, semaphore_test_waiter_task),
    RtTask::with_priority("sem-post", 0, 12, semaphore_test_poster_task),
];

/// Axvisor realtime secondary CPU entry.
///
/// This symbol is called by `ax-runtime` after the reserved CPU has completed
/// minimal secondary CPU-local initialization and before it can enter the normal
/// host scheduler path.
#[unsafe(no_mangle)]
pub extern "Rust" fn ax_realtime_secondary_main(cpu_id: usize) -> ! {
    let entry_nanos = monotonic_time_nanos();
    RT_LAST_HEARTBEAT_NANOS.store(entry_nanos, Ordering::Release);
    RT_LAST_WATCHDOG_NANOS.store(entry_nanos, Ordering::Release);

    info!("Realtime CPU {cpu_id} entered Axvisor RT entry; running isolated executor.");
    ax_rt::run_realtime_cpu(cpu_id, &RT_TASKS, monotonic_time_nanos)
}

fn heartbeat_task() -> ! {
    let mut next_deadline = monotonic_time_nanos();
    loop {
        let now = monotonic_time_nanos();
        {
            let _guard = RT_SAMPLE_MUTEX.lock();
            RT_HEARTBEATS.fetch_add(1, Ordering::Relaxed);
            RT_LAST_HEARTBEAT_NANOS.store(now, Ordering::Release);
        }
        next_deadline = next_deadline.saturating_add(HEARTBEAT_INTERVAL_NANOS);
        if next_deadline <= monotonic_time_nanos() {
            ax_rt::rt_yield_now();
        } else {
            rt_delay_until(next_deadline);
        }
    }
}

fn watchdog_task() -> ! {
    loop {
        let now = monotonic_time_nanos();
        {
            let _guard = RT_SAMPLE_MUTEX.lock();
            RT_WATCHDOG_RUNS.fetch_add(1, Ordering::Relaxed);
            RT_LAST_WATCHDOG_NANOS.store(now, Ordering::Release);
        }
        rt_sleep(WATCHDOG_INTERVAL_NANOS);
    }
}

fn hello_task() -> ! {
    for index in 1..=HELLO_RUNS {
        rt_output_write(b"hello from RT task ");
        ax_rt::rt_output_write_decimal(index);
        rt_output_write(b"/5\n");
        rt_sleep(HELLO_INTERVAL_NANOS);
    }
    rt_exit_current_task();
}

fn priority_test_low_task() -> ! {
    {
        let _guard = RT_PRIORITY_TEST_MUTEX.lock();
        RT_PRIORITY_TEST_STEP.store(PRIORITY_TEST_LOW_READY, Ordering::Release);
        while RT_PRIORITY_TEST_STEP.load(Ordering::Acquire) != PRIORITY_TEST_HIGH_BLOCKED {
            ax_rt::rt_yield_now();
        }
        RT_PRIORITY_TEST_STEP.store(PRIORITY_TEST_LOW_RELEASED, Ordering::Release);
    }
    rt_exit_current_task();
}

fn priority_test_high_task() -> ! {
    rt_sleep(1_000_000);
    while RT_PRIORITY_TEST_STEP.load(Ordering::Acquire) != PRIORITY_TEST_LOW_READY {
        ax_rt::rt_yield_now();
    }
    RT_PRIORITY_TEST_STEP.store(PRIORITY_TEST_HIGH_BLOCKED, Ordering::Release);
    {
        let _guard = RT_PRIORITY_TEST_MUTEX.lock();
        if RT_PRIORITY_TEST_STEP.load(Ordering::Acquire) == PRIORITY_TEST_MEDIUM_RAN {
            RT_PRIORITY_TEST_RESULT.store(2, Ordering::Release);
        } else {
            RT_PRIORITY_TEST_STEP.store(PRIORITY_TEST_HIGH_ACQUIRED, Ordering::Release);
            RT_PRIORITY_TEST_RESULT.store(1, Ordering::Release);
        }
    }
    rt_exit_current_task();
}

fn priority_test_medium_task() -> ! {
    rt_sleep(1_000_000);
    while RT_PRIORITY_TEST_STEP.load(Ordering::Acquire) < PRIORITY_TEST_HIGH_BLOCKED {
        ax_rt::rt_yield_now();
    }
    if RT_PRIORITY_TEST_STEP.load(Ordering::Acquire) == PRIORITY_TEST_HIGH_BLOCKED {
        RT_PRIORITY_TEST_STEP.store(PRIORITY_TEST_MEDIUM_RAN, Ordering::Release);
    }
    rt_exit_current_task();
}

fn recursive_test_owner_task() -> ! {
    rt_sleep(2_000_000);
    {
        let _outer = RT_RECURSIVE_TEST_MUTEX.lock();
        {
            let _inner = RT_RECURSIVE_TEST_MUTEX.lock();
            RT_RECURSIVE_TEST_STEP.store(RECURSIVE_TEST_OWNER_READY, Ordering::Release);
            rt_sleep(1_000_000);
            if RT_RECURSIVE_TEST_STEP.load(Ordering::Acquire) == RECURSIVE_TEST_WAITER_ACQUIRED {
                RT_RECURSIVE_TEST_RESULT.store(2, Ordering::Release);
            }
            while RT_RECURSIVE_TEST_STEP.load(Ordering::Acquire) != RECURSIVE_TEST_WAITER_BLOCKING {
                rt_sleep(100_000);
            }
        }
        RT_RECURSIVE_TEST_STEP.store(RECURSIVE_TEST_INNER_DROPPED, Ordering::Release);
        rt_sleep(1_000_000);
        if RT_RECURSIVE_TEST_STEP.load(Ordering::Acquire) == RECURSIVE_TEST_WAITER_ACQUIRED {
            RT_RECURSIVE_TEST_RESULT.store(2, Ordering::Release);
        }
    }
    while RT_RECURSIVE_TEST_STEP.load(Ordering::Acquire) != RECURSIVE_TEST_WAITER_ACQUIRED {
        rt_sleep(100_000);
    }
    if RT_RECURSIVE_TEST_RESULT.load(Ordering::Acquire) == 0 {
        RT_RECURSIVE_TEST_RESULT.store(1, Ordering::Release);
    }
    rt_exit_current_task();
}

fn recursive_test_waiter_task() -> ! {
    rt_sleep(2_500_000);
    while RT_RECURSIVE_TEST_STEP.load(Ordering::Acquire) < RECURSIVE_TEST_OWNER_READY {
        rt_sleep(100_000);
    }
    RT_RECURSIVE_TEST_STEP.store(RECURSIVE_TEST_WAITER_BLOCKING, Ordering::Release);
    {
        let _guard = RT_RECURSIVE_TEST_MUTEX.lock();
        RT_RECURSIVE_TEST_STEP.store(RECURSIVE_TEST_WAITER_ACQUIRED, Ordering::Release);
    }
    rt_exit_current_task();
}

/// Blocks on an empty semaphore, then records that it was woken by a later
/// [`RtSemaphore::release`] from the poster task.
///
/// The store to `RT_SEMAPHORE_TEST_STEP` happens immediately before the
/// blocking `acquire()`. Because the RT executor is cooperative and single-CPU,
/// no other RT task runs between the store and the block, so a poster that
/// observes `SEMAPHORE_TEST_WAITER_BLOCKING` knows this task is already parked
/// on the semaphore.
fn semaphore_test_waiter_task() -> ! {
    rt_sleep(2_000_000);
    RT_SEMAPHORE_TEST_STEP.store(SEMAPHORE_TEST_WAITER_BLOCKING, Ordering::Release);
    RT_SEMAPHORE_TEST_SEM.acquire();
    RT_SEMAPHORE_TEST_STEP.store(SEMAPHORE_TEST_WAITER_ACQUIRED, Ordering::Release);
    rt_exit_current_task();
}

/// Waits until the waiter has blocked on the empty semaphore, verifies it did
/// not acquire a permit that was never released, then releases one permit and
/// confirms the blocked waiter is woken.
///
/// Result codes: `1` PASS, `2` FAIL (acquired without a permit), `3` FAIL
/// (release did not wake the blocked waiter).
fn semaphore_test_poster_task() -> ! {
    rt_sleep(2_500_000);
    while RT_SEMAPHORE_TEST_STEP.load(Ordering::Acquire) < SEMAPHORE_TEST_WAITER_BLOCKING {
        rt_sleep(100_000);
    }
    if RT_SEMAPHORE_TEST_STEP.load(Ordering::Acquire) == SEMAPHORE_TEST_WAITER_ACQUIRED {
        RT_SEMAPHORE_TEST_RESULT.store(2, Ordering::Release);
        rt_exit_current_task();
    }
    RT_SEMAPHORE_TEST_SEM.release();
    let deadline_nanos = monotonic_time_nanos().saturating_add(1_000_000_000);
    while RT_SEMAPHORE_TEST_STEP.load(Ordering::Acquire) != SEMAPHORE_TEST_WAITER_ACQUIRED {
        if monotonic_time_nanos() >= deadline_nanos {
            RT_SEMAPHORE_TEST_RESULT.store(3, Ordering::Release);
            rt_exit_current_task();
        }
        rt_sleep(100_000);
    }
    RT_SEMAPHORE_TEST_RESULT.store(1, Ordering::Release);
    rt_exit_current_task();
}

pub fn log_priority_test_result() {
    let mut priority_done = false;
    let mut recursive_done = false;
    let mut semaphore_done = false;
    let deadline_nanos = monotonic_time_nanos().saturating_add(5_000_000_000);
    while monotonic_time_nanos() < deadline_nanos {
        if !priority_done {
            match RT_PRIORITY_TEST_RESULT.load(Ordering::Acquire) {
                1 => {
                    info!("[RT priority test] priority inheritance PASS");
                    priority_done = true;
                }
                2 => {
                    error!("[RT priority test] medium task ran before inherited owner FAIL");
                    priority_done = true;
                }
                _ => {}
            }
        }
        if !recursive_done {
            match RT_RECURSIVE_TEST_RESULT.load(Ordering::Acquire) {
                1 => {
                    info!("[RT recursive mutex test] recursive lock PASS");
                    recursive_done = true;
                }
                2 => {
                    error!("[RT recursive mutex test] waiter entered before outer unlock FAIL");
                    recursive_done = true;
                }
                _ => {}
            }
        }
        if !semaphore_done {
            match RT_SEMAPHORE_TEST_RESULT.load(Ordering::Acquire) {
                1 => {
                    info!("[RT semaphore test] signal wakes blocked waiter PASS");
                    semaphore_done = true;
                }
                2 => {
                    error!("[RT semaphore test] acquire returned without a permit FAIL");
                    semaphore_done = true;
                }
                3 => {
                    error!("[RT semaphore test] release did not wake blocked waiter FAIL");
                    semaphore_done = true;
                }
                _ => {}
            }
        }
        if priority_done && recursive_done && semaphore_done {
            return;
        }
        core::hint::spin_loop();
    }
    if !priority_done {
        warn!("[RT priority test] timed out waiting for realtime task result");
    }
    if !recursive_done {
        warn!(
            "[RT recursive mutex test] timed out waiting for realtime task result: step={}, result={}",
            RT_RECURSIVE_TEST_STEP.load(Ordering::Acquire),
            RT_RECURSIVE_TEST_RESULT.load(Ordering::Acquire)
        );
    }
    if !semaphore_done {
        warn!(
            "[RT semaphore test] timed out waiting for realtime task result: step={}, result={}",
            RT_SEMAPHORE_TEST_STEP.load(Ordering::Acquire),
            RT_SEMAPHORE_TEST_RESULT.load(Ordering::Acquire)
        );
    }
}

/// Returns the number of Axvisor demo heartbeat periods observed on the RT CPU.
pub fn heartbeats() -> u64 {
    RT_HEARTBEATS.load(Ordering::Relaxed)
}

/// Returns the latest Axvisor demo heartbeat timestamp.
pub fn last_heartbeat_nanos() -> u64 {
    RT_LAST_HEARTBEAT_NANOS.load(Ordering::Acquire)
}

/// Returns the latest Axvisor demo watchdog timestamp.
pub fn last_watchdog_nanos() -> u64 {
    RT_LAST_WATCHDOG_NANOS.load(Ordering::Acquire)
}

fn monotonic_time_nanos() -> u64 {
    ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos()
}

/// Runtime owner of a physical CPU.
#[cfg(feature = "realtime")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuOwner {
    /// CPU is owned by the ordinary Axvisor host runtime.
    Host,
    /// CPU is reserved for the realtime runtime.
    Realtime,
    /// CPU is deliberately parked and not used by either runtime.
    Offline,
}

/// Returns the owner for `cpu_id`.
#[cfg(feature = "realtime")]
pub fn cpu_owner(cpu_id: usize) -> CpuOwner {
    if cpu_id >= runtime_cpu_count() {
        return CpuOwner::Offline;
    }
    if configured_realtime_cpu() == Some(cpu_id) {
        return CpuOwner::Realtime;
    }

    CpuOwner::Host
}

/// Logs the CPU ownership partition selected for this Axvisor build.
#[cfg(feature = "realtime")]
pub fn log_cpu_partition() {
    info!(
        "Axvisor realtime CPU partition: host_cpus={}, runtime_cpus={}",
        host_cpu_count(),
        runtime_cpu_count()
    );
    for cpu_id in 0..runtime_cpu_count() {
        debug!("  pCPU{cpu_id}: {:?}", cpu_owner(cpu_id));
    }
}

/// Returns whether `cpu_id` belongs to the ordinary Axvisor host runtime.
#[cfg(feature = "realtime")]
pub fn is_host_cpu(cpu_id: usize) -> bool {
    cpu_owner(cpu_id) == CpuOwner::Host
}

/// Returns the number of CPUs visible to the ordinary Axvisor host runtime.
#[cfg(feature = "realtime")]
pub fn host_cpu_count() -> usize {
    (0..runtime_cpu_count())
        .filter(|&cpu_id| is_host_cpu(cpu_id))
        .count()
}

#[cfg(feature = "realtime")]
fn runtime_cpu_count() -> usize {
    ax_std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
}

#[cfg(feature = "realtime")]
fn configured_realtime_cpu() -> Option<usize> {
    option_env!("AX_RT_CPU").and_then(parse_cpu_id)
}

#[cfg(feature = "realtime")]
fn parse_cpu_id(value: &str) -> Option<usize> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix("0x") {
        usize::from_str_radix(hex, 16).ok()
    } else {
        value.parse().ok()
    }
}

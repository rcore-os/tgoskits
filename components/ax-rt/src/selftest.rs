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

//! Self-test suite for the RT executor's cooperative primitives.
//!
//! The suite validates behaviour that a host kernel cannot easily assert from
//! the outside: priority inheritance through [`RtMutex`], recursive locking,
//! [`RtSemaphore`] wakeups, and a host↔RT mailbox round-trip. It lives here,
//! behind the `selftest` feature, so every host (Axvisor, StarryOS, …) runs the
//! same suite instead of copying the test tasks into each integration crate.
//!
//! Split of responsibilities:
//!
//! - [`SELFTEST_TASKS`] are the RT-side tasks a host appends to its RT task
//!   table. They only touch atomics and RT primitives and never log, so the
//!   isolated RT core never contends on the host-owned console lock.
//! - [`run_host_checks`] runs on the host core: it drives the mailbox exchange,
//!   observes the result atomics, and logs the PASS/FAIL lines. Everything
//!   HAL-specific (a monotonic clock, reporting the reverse doorbell IPI) is
//!   injected through [`SelftestConfig`] so this module stays HAL-free.

use core::sync::atomic::{AtomicU64, Ordering};

use log::{error, info, warn};

use crate::{
    RtMessage, RtMutex, RtSemaphore, RtTask, host_mailbox_recv, host_mailbox_send,
    rt_exit_current_task, rt_mailbox_recv, rt_mailbox_send, rt_mailbox_stats,
    rt_mailbox_take_pending, rt_monotonic_nanos, rt_sleep, rt_yield_now,
};

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
/// host→RT command tag: echo the payload back as an event.
const MAILBOX_CMD_ECHO: u32 = 0x01;
/// RT→host event tag reported by the echo task (command tag | 0x80).
const MAILBOX_EVT_ECHO: u32 = MAILBOX_CMD_ECHO | 0x80;
/// Payload the host round-trip test sends and expects echoed back.
const MAILBOX_TEST_PAYLOAD: &[u8] = b"rt-mailbox-ping";

static RT_PRIORITY_TEST_MUTEX: RtMutex = RtMutex::new();
static RT_PRIORITY_TEST_STEP: AtomicU64 = AtomicU64::new(0);
static RT_PRIORITY_TEST_RESULT: AtomicU64 = AtomicU64::new(0);
static RT_RECURSIVE_TEST_MUTEX: RtMutex = RtMutex::new();
static RT_RECURSIVE_TEST_STEP: AtomicU64 = AtomicU64::new(0);
static RT_RECURSIVE_TEST_RESULT: AtomicU64 = AtomicU64::new(0);
static RT_SEMAPHORE_TEST_SEM: RtSemaphore = RtSemaphore::new(0);
static RT_SEMAPHORE_TEST_STEP: AtomicU64 = AtomicU64::new(0);
static RT_SEMAPHORE_TEST_RESULT: AtomicU64 = AtomicU64::new(0);
static RT_MAILBOX_TEST_RESULT: AtomicU64 = AtomicU64::new(0);

/// RT-side tasks that make up the self-test suite.
///
/// A host appends these to its own RT task table (after its demo/service tasks)
/// when the `selftest` feature is enabled. The ordering and priorities are load
/// bearing: the priority-inheritance case depends on the relative priorities of
/// the low/medium/high tasks.
pub const SELFTEST_TASKS: [RtTask; 8] = [
    RtTask::with_priority("prio-low", 0, 20, priority_test_low_task),
    RtTask::with_priority("prio-high", 0, 40, priority_test_high_task),
    RtTask::with_priority("prio-mid", 0, 30, priority_test_medium_task),
    RtTask::with_priority("recur-own", 0, 25, recursive_test_owner_task),
    RtTask::with_priority("recur-wait", 0, 15, recursive_test_waiter_task),
    RtTask::with_priority("sem-wait", 0, 35, semaphore_test_waiter_task),
    RtTask::with_priority("sem-post", 0, 12, semaphore_test_poster_task),
    RtTask::with_priority("mbox-echo", 0, 8, mailbox_echo_task),
];

fn priority_test_low_task() -> ! {
    {
        let _guard = RT_PRIORITY_TEST_MUTEX.lock();
        RT_PRIORITY_TEST_STEP.store(PRIORITY_TEST_LOW_READY, Ordering::Release);
        while RT_PRIORITY_TEST_STEP.load(Ordering::Acquire) != PRIORITY_TEST_HIGH_BLOCKED {
            rt_yield_now();
        }
        RT_PRIORITY_TEST_STEP.store(PRIORITY_TEST_LOW_RELEASED, Ordering::Release);
    }
    rt_exit_current_task();
}

fn priority_test_high_task() -> ! {
    rt_sleep(1_000_000);
    while RT_PRIORITY_TEST_STEP.load(Ordering::Acquire) != PRIORITY_TEST_LOW_READY {
        rt_yield_now();
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
        rt_yield_now();
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
    let deadline_nanos = rt_monotonic_nanos().saturating_add(1_000_000_000);
    while RT_SEMAPHORE_TEST_STEP.load(Ordering::Acquire) != SEMAPHORE_TEST_WAITER_ACQUIRED {
        if rt_monotonic_nanos() >= deadline_nanos {
            RT_SEMAPHORE_TEST_RESULT.store(3, Ordering::Release);
            rt_exit_current_task();
        }
        rt_sleep(100_000);
    }
    RT_SEMAPHORE_TEST_RESULT.store(1, Ordering::Release);
    rt_exit_current_task();
}

/// Long-running RT service task: drains host→RT commands and echoes each one
/// back to the host as an RT→host event (`tag | 0x80`, same payload).
///
/// Consuming the mailbox pending flag makes the poll loop cheap once IPI-driven
/// notification is wired; today it also polls on a short period as a fallback.
fn mailbox_echo_task() -> ! {
    loop {
        let _woken = rt_mailbox_take_pending();
        while let Some(command) = rt_mailbox_recv() {
            if let Ok(reply) = RtMessage::new(command.tag() | 0x80, command.payload()) {
                // Best-effort: if the RT→host ring is full the host will observe
                // the drop counter; the RT task must not block.
                let _ = rt_mailbox_send(&reply);
            }
        }
        rt_sleep(500_000);
    }
}

/// Host-injected capabilities the self-test driver needs but this crate must not
/// hard-code: a monotonic clock and a hook that reports the reverse doorbell IPI.
pub struct SelftestConfig {
    /// Monotonic time source, in nanoseconds.
    pub time_fn: fn() -> u64,
    /// Called with the observed RT→host doorbell notification count just before
    /// the mailbox round-trip result is logged. A host uses it to log the
    /// architecture-specific reverse-IPI observation; a no-op is fine.
    pub report_reverse_doorbell: fn(u64),
}

/// Drives the self-test suite from the host core and logs each PASS/FAIL line.
///
/// Must run on the host core that also drains the RT→host mailbox ring. Sends
/// the mailbox round-trip ping, then polls the result atomics the RT tasks
/// publish until every case reports or a 5-second budget elapses.
pub fn run_host_checks(config: &SelftestConfig) {
    let now = config.time_fn;
    let mut priority_done = false;
    let mut recursive_done = false;
    let mut semaphore_done = false;
    let mut mailbox_done = false;
    let mailbox_ping =
        RtMessage::new(MAILBOX_CMD_ECHO, MAILBOX_TEST_PAYLOAD).expect("mailbox test payload fits");
    let mut mailbox_sent = host_mailbox_send(&mailbox_ping).is_ok();
    let deadline_nanos = now().saturating_add(5_000_000_000);
    while now() < deadline_nanos {
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
        if !mailbox_done {
            if !mailbox_sent {
                mailbox_sent = host_mailbox_send(&mailbox_ping).is_ok();
            }
            if let Some(reply) = host_mailbox_recv() {
                if reply.tag() == MAILBOX_EVT_ECHO && reply.payload() == MAILBOX_TEST_PAYLOAD {
                    // The RT→host reply is enqueued just before the reverse
                    // doorbell fires, so the host can pop it before that SGI is
                    // delivered here. Wait briefly for the notification counter
                    // to catch up so the logged count reflects the real IPI.
                    let mut stats = rt_mailbox_stats();
                    let ipi_deadline_nanos = now().saturating_add(10_000_000);
                    while stats.host_notifications == 0 && now() < ipi_deadline_nanos {
                        core::hint::spin_loop();
                        stats = rt_mailbox_stats();
                    }
                    // Report the reverse IPI from the host side (the RT core is
                    // the only sender of this doorbell SGI) so both directions
                    // are visible without logging from the isolated RT core.
                    (config.report_reverse_doorbell)(stats.host_notifications);
                    info!(
                        "[RT mailbox test] host->RT->host round-trip PASS (rt_notifications={}, \
                         host_notifications={})",
                        stats.rt_notifications, stats.host_notifications
                    );
                    RT_MAILBOX_TEST_RESULT.store(1, Ordering::Release);
                } else {
                    error!("[RT mailbox test] echoed message mismatch FAIL");
                    RT_MAILBOX_TEST_RESULT.store(2, Ordering::Release);
                }
                mailbox_done = true;
            }
        }
        if priority_done && recursive_done && semaphore_done && mailbox_done {
            return;
        }
        core::hint::spin_loop();
    }
    if !priority_done {
        warn!("[RT priority test] timed out waiting for realtime task result");
    }
    if !recursive_done {
        warn!(
            "[RT recursive mutex test] timed out waiting for realtime task result: step={}, \
             result={}",
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
    if !mailbox_done {
        let stats = rt_mailbox_stats();
        warn!(
            "[RT mailbox test] timed out waiting for echo: sent={mailbox_sent}, to_rt_depth={}, \
             to_host_depth={}, to_rt_dropped={}, to_host_dropped={}",
            stats.to_rt_depth, stats.to_host_depth, stats.to_rt_dropped, stats.to_host_dropped
        );
    }
}

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

use ax_rt::{RtMutex, RtTask, rt_delay_until, rt_exit_current_task, rt_output_write, rt_sleep};
pub use ax_rt::{RtState, RtTaskState, rt_read_output, status};

const HEARTBEAT_INTERVAL_NANOS: u64 = 1_000_000;
const WATCHDOG_INTERVAL_NANOS: u64 = 100_000_000;
const HELLO_INTERVAL_NANOS: u64 = 1_000_000_000;
const HELLO_RUNS: u64 = 5;

static RT_HEARTBEATS: AtomicU64 = AtomicU64::new(0);
static RT_WATCHDOG_RUNS: AtomicU64 = AtomicU64::new(0);
static RT_LAST_HEARTBEAT_NANOS: AtomicU64 = AtomicU64::new(0);
static RT_LAST_WATCHDOG_NANOS: AtomicU64 = AtomicU64::new(0);
static RT_SAMPLE_MUTEX: RtMutex = RtMutex::new();
static RT_TASKS: [RtTask; 3] = [
    RtTask::new("heartbeat", HEARTBEAT_INTERVAL_NANOS, heartbeat_task),
    RtTask::new("watchdog", WATCHDOG_INTERVAL_NANOS, watchdog_task),
    RtTask::new("hello", HELLO_INTERVAL_NANOS, hello_task),
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

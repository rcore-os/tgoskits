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
//!
//! The first implementation keeps the default Axvisor behavior unchanged: every
//! runtime CPU belongs to the host scheduler unless `AX_RT_CPU` is set at build
//! time. Later phases will replace the parking entry with a realtime executor.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

const HEARTBEAT_INTERVAL_NANOS: u64 = 10_000_000;

static RT_CPU_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
static RT_STATE: AtomicUsize = AtomicUsize::new(RtState::Offline as usize);
static RT_HEARTBEATS: AtomicU64 = AtomicU64::new(0);
static RT_ENTRY_NANOS: AtomicU64 = AtomicU64::new(0);
static RT_LAST_HEARTBEAT_NANOS: AtomicU64 = AtomicU64::new(0);

/// Snapshot of the realtime CPU runtime state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtStatus {
    /// Reserved realtime CPU ID, or `None` before the RT entry runs.
    pub cpu_id: Option<usize>,
    /// Current runtime state.
    pub state: RtState,
    /// Number of heartbeat periods observed by the RT loop.
    pub heartbeats: u64,
    /// Monotonic timestamp when the RT entry started.
    pub entry_nanos: u64,
    /// Monotonic timestamp of the latest heartbeat.
    pub last_heartbeat_nanos: u64,
}

/// Realtime CPU entry state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum RtState {
    /// The realtime CPU has not entered Axvisor yet.
    Offline = 0,
    /// The realtime CPU is executing the temporary heartbeat loop.
    Heartbeat = 1,
}

/// Axvisor realtime secondary CPU entry.
///
/// This symbol is called by `ax-runtime` after the reserved CPU has completed
/// minimal secondary CPU-local initialization and before it can enter the normal
/// host scheduler path.
#[unsafe(no_mangle)]
pub extern "Rust" fn ax_realtime_secondary_main(cpu_id: usize) -> ! {
    let entry_nanos = monotonic_time_nanos();
    RT_CPU_ID.store(cpu_id, Ordering::Release);
    RT_ENTRY_NANOS.store(entry_nanos, Ordering::Release);
    RT_LAST_HEARTBEAT_NANOS.store(entry_nanos, Ordering::Release);
    RT_STATE.store(RtState::Heartbeat as usize, Ordering::Release);

    info!("Realtime CPU {cpu_id} entered Axvisor RT entry; running heartbeat loop.");
    let mut next_heartbeat = entry_nanos.saturating_add(HEARTBEAT_INTERVAL_NANOS);
    loop {
        let now = monotonic_time_nanos();
        if now >= next_heartbeat {
            RT_HEARTBEATS.fetch_add(1, Ordering::Relaxed);
            RT_LAST_HEARTBEAT_NANOS.store(now, Ordering::Release);
            next_heartbeat = now.saturating_add(HEARTBEAT_INTERVAL_NANOS);
        }
        core::hint::spin_loop();
    }
}

/// Returns the latest realtime CPU status snapshot.
pub fn status() -> RtStatus {
    let cpu_id = match RT_CPU_ID.load(Ordering::Acquire) {
        usize::MAX => None,
        cpu_id => Some(cpu_id),
    };

    RtStatus {
        cpu_id,
        state: rt_state_from_usize(RT_STATE.load(Ordering::Acquire)),
        heartbeats: RT_HEARTBEATS.load(Ordering::Relaxed),
        entry_nanos: RT_ENTRY_NANOS.load(Ordering::Acquire),
        last_heartbeat_nanos: RT_LAST_HEARTBEAT_NANOS.load(Ordering::Acquire),
    }
}

fn rt_state_from_usize(value: usize) -> RtState {
    match value {
        value if value == RtState::Heartbeat as usize => RtState::Heartbeat,
        _ => RtState::Offline,
    }
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

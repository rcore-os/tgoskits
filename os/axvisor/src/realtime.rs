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

/// Axvisor realtime secondary CPU entry.
///
/// This symbol is called by `ax-runtime` after the reserved CPU has completed
/// minimal secondary CPU-local initialization and before it can enter the normal
/// host scheduler path.
#[unsafe(no_mangle)]
pub extern "Rust" fn ax_realtime_secondary_main(cpu_id: usize) -> ! {
    info!("Realtime CPU {cpu_id} entered Axvisor RT entry; parking until executor is installed.");
    loop {
        ax_std::os::arceos::modules::ax_hal::asm::wait_for_irqs();
    }
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

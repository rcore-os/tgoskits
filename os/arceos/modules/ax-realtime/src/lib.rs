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

//! ArceOS realtime-core integration shared by Axvisor and StarryOS.
//!
//! The reusable realtime executor, mailbox data plane, and cooperative
//! primitives live in [`ax_rt`] and are OS- and HAL-free. This crate is the
//! ArceOS glue that a host kernel needs to actually run a reserved realtime
//! core: it arms the mailbox notification plane (the doorbell IPIs) using the
//! ArceOS HAL and drives the executor.
//!
//! A host kernel provides an [`RtConfig`] (its RT task table and time source)
//! from its own `ax_realtime_secondary_main` entry symbol and calls [`run`];
//! the boot CPU calls [`setup_host_side`]. Everything architecture-specific is
//! confined to the doorbell notification plane behind the [`DoorbellArch`]
//! capability, so adding an architecture only touches one `imp_<arch>` module.

#![no_std]

/// Architecture backend for the host↔RT mailbox doorbell notification plane.
///
/// The mailbox data plane and executor ([`ax_rt`]) are architecture-free; the
/// only per-architecture concern is how the two cores signal each other. A
/// doorbell is a cross-core interrupt in each direction:
///
/// - **host→RT**: after `host_mailbox_send`, the host rings the reserved core so
///   it drains the `to_rt` ring without busy-polling.
/// - **RT→host**: after `rt_mailbox_send`, the RT core rings the host consumer
///   core so it drains the `to_host` ring.
///
/// A backend registers the receive-side interrupt handlers on each core (each
/// handler does nothing but set the mailbox pending flag) and installs the
/// send-side [`ax_rt::MailboxDoorbell`] objects. Backends are stateless unit
/// types; any per-core state lives in the backend module's statics. When an
/// architecture has no doorbell yet, [`imp_fallback`] leaves both directions on
/// the executor's poll fallback.
trait DoorbellArch {
    /// Arms the RT core's incoming (host→RT) doorbell and installs the sender
    /// used by `host_mailbox_send`. Runs once on the reserved RT core, from
    /// [`run`], before the executor loop starts.
    fn setup_rt_side(cpu_id: usize);

    /// Arms the host core's incoming (RT→host) doorbell and installs the sender
    /// used by `rt_mailbox_send`. Runs once on the host consumer core, from
    /// [`setup_host_side`].
    fn setup_host_side();

    /// Logs whether the host observed the RT→host doorbell interrupt. A nonzero
    /// count means the reverse path delivered a real interrupt; zero means it
    /// silently fell back to polling. Called from the host self-test after a
    /// round-trip completes.
    fn report_reverse_doorbell(host_notifications: u64);
}

#[cfg(target_arch = "aarch64")]
mod imp_aarch64;
#[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
mod imp_fallback;
#[cfg(target_arch = "riscv64")]
mod imp_riscv64;

/// The doorbell backend selected for this build's target architecture.
#[cfg(target_arch = "aarch64")]
use imp_aarch64::Doorbell as ActiveDoorbell;
#[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
use imp_fallback::Doorbell as ActiveDoorbell;
#[cfg(target_arch = "riscv64")]
use imp_riscv64::Doorbell as ActiveDoorbell;

/// Configuration a host kernel supplies to run its reserved realtime core.
pub struct RtConfig {
    /// The realtime task table executed on the reserved core.
    pub tasks: &'static [ax_rt::RtTask],
    /// Monotonic time source, in nanoseconds, used by the executor.
    pub time_fn: fn() -> u64,
}

/// Realtime secondary-core entry: arm the RT-side mailbox doorbell, then run the
/// isolated executor forever.
///
/// Call this from the kernel's `ax_realtime_secondary_main` symbol, which
/// `ax-runtime` invokes on the reserved core after minimal secondary init.
pub fn run(cpu_id: usize, config: &RtConfig) -> ! {
    ActiveDoorbell::setup_rt_side(cpu_id);
    ax_rt::run_realtime_cpu(cpu_id, config.tasks, config.time_fn)
}

/// Arms interrupt-driven RT→host mailbox notification on the current host core.
///
/// Call this once from the host boot CPU, which is also the core that drains the
/// RT→host ring (`host_mailbox_recv`).
pub fn setup_host_side() {
    ActiveDoorbell::setup_host_side();
}

/// Logs whether the host observed the RT core's reverse doorbell interrupt.
///
/// Call from the host self-test after the round-trip completes. Delegates to the
/// active [`DoorbellArch`] backend, which knows the architecture-specific
/// interrupt line; on architectures without a doorbell this is a no-op.
pub fn report_reverse_doorbell(host_notifications: u64) {
    ActiveDoorbell::report_reverse_doorbell(host_notifications);
}

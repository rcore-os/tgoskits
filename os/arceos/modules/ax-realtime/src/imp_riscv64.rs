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

//! riscv64 doorbell backend: the per-hart supervisor software interrupt (SSWI).
//!
//! RISC-V exposes a single per-hart software interrupt (`ipi_irq()`, the
//! supervisor soft cause driven by ACLINT/CLINT SSWI); the platform's `send_ipi`
//! refuses any other line. Unlike aarch64 there is no spare line for a second
//! direction, so this backend arms only the host→RT direction and leaves RT→host
//! on the executor's poll fallback:
//!
//! - **host→RT**: the host rings the reserved RT hart's software interrupt; the
//!   RT hart's handler sets the mailbox pending flag. Interrupt-driven.
//! - **RT→host**: the host hart's software interrupt is already owned by the
//!   scheduler IPI (`init_percpu_irq` registers it on the host CPU set), so the
//!   reverse doorbell stays poll-based. Model B (SG2002) will add a dedicated
//!   carveout mailbox interrupt for this direction.
//!
//! This is sound because the scheduler IPI and the doorbell are disjoint
//! per-CPU actions on the one shared software-interrupt line: `axruntime`
//! registers the scheduler IPI as a *shared* per-CPU action scoped to the host
//! CPUs, and this backend attaches a second shared per-CPU action scoped to the
//! reserved RT hart alone. Each fires only on the CPUs in its own mask, and
//! `send_ipi` targets a specific hart, so a host→RT doorbell never disturbs the
//! host scheduler and vice versa. (A plain exclusive per-CPU request on the
//! already-registered line would be refused with `Busy`.)
//! See the crate-level [`DoorbellArch`](crate::DoorbellArch) contract.

use core::sync::atomic::{AtomicUsize, Ordering};

use log::{info, warn};

use crate::DoorbellArch;

/// riscv64 SSWI doorbell backend. Zero-sized; per-core state lives in this
/// module's statics.
pub struct Doorbell;

impl DoorbellArch for Doorbell {
    fn setup_rt_side(cpu_id: usize) {
        setup_rt_mailbox_doorbell(cpu_id);
    }

    fn setup_host_side() {
        // RT→host reverse doorbell would need a second software-interrupt line,
        // but RISC-V exposes only one per hart and the host's is already the
        // scheduler IPI. The reverse direction therefore stays poll-based; there
        // is nothing to arm on the host core here.
    }

    fn report_reverse_doorbell(host_notifications: u64) {
        report_reverse_doorbell(host_notifications);
    }
}

/// Reserved RT hart, i.e. the target of the host→RT doorbell. Set once when the
/// RT hart arms its doorbell.
static RT_MAILBOX_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Doorbell that rings the reserved RT hart after a host→RT send.
struct RtCoreDoorbell;

impl ax_rt::MailboxDoorbell for RtCoreDoorbell {
    fn ring(&self) {
        use ax_hal::{irq, percpu};
        let cpu = RT_MAILBOX_CPU.load(Ordering::Acquire);
        if cpu == usize::MAX {
            return;
        }
        // Runs on a host core, which owns the shared console, so logging the
        // outgoing doorbell here is safe (unlike the RT core's reverse path).
        info!(
            "[RT mailbox] doorbell IPI: host CPU{} -> RT CPU{cpu} (SSWI)",
            percpu::this_cpu_id()
        );
        irq::send_ipi(irq::ipi_irq(), irq::IpiTarget::Other { cpu_id: cpu });
    }
}

static RT_CORE_DOORBELL: RtCoreDoorbell = RtCoreDoorbell;

/// Enables interrupt-driven host→RT mailbox notification on the reserved RT hart.
///
/// The RT hart deliberately skips the ordinary secondary IRQ-online path, so it
/// brings itself online and registers just this one software-interrupt handler
/// here, as a shared per-CPU action scoped to the RT hart. The scheduler IPI on
/// the same line is a shared per-CPU action scoped to the host CPU set, so the
/// two coexist and each fires only on its own cores. The handler runs in
/// interrupt context and does nothing but set the mailbox pending flag.
fn setup_rt_mailbox_doorbell(cpu_id: usize) {
    use ax_hal::irq;

    RT_MAILBOX_CPU.store(cpu_id, Ordering::Release);
    irq::init_common_irq_handler();
    if let Err(err) = irq::cpu_online(cpu_id) {
        warn!("RT mailbox doorbell: cpu_online({cpu_id}) failed: {err:?}");
        return;
    }
    let doorbell_irq = irq::ipi_irq();
    let doorbell_cpus = irq::CpuMask::from_cpu(irq::CpuId(cpu_id));
    // Share the single supervisor software-interrupt line with the scheduler
    // IPI: the runtime registers that IPI as a shared per-CPU action on the host
    // CPUs, and this is a second shared per-CPU action scoped to the reserved RT
    // hart alone. The two masks are disjoint, so each fires only on its own
    // cores. A plain `request_percpu_irq` would make an exclusive descriptor and
    // be refused with `Busy`, silently dropping host->RT to poll fallback.
    let result = irq::request_percpu_shared_irq(doorbell_irq, doorbell_cpus, |_ctx| {
        ax_rt::rt_mailbox_on_doorbell();
        irq::IrqReturn::Handled
    });
    if let Err(err) = result {
        warn!("RT mailbox doorbell: request_percpu_irq failed: {err:?}; host->RT stays poll-based");
        return;
    }
    // From now on host_mailbox_send() rings this hart instead of relying on the
    // RT task's fallback poll.
    ax_rt::set_rt_doorbell(&RT_CORE_DOORBELL);
    ax_hal::asm::enable_irqs();
    info!("RT mailbox doorbell armed on CPU {cpu_id} (SSWI).");
}

/// Reports the RT→host reverse-notification path status.
///
/// On riscv64 the reverse direction is poll-based by design (only one
/// software-interrupt line per hart, and the host's is the scheduler IPI), so a
/// zero count here is expected and not an error. The round-trip still completes
/// via the host's poll of the `to_host` ring.
fn report_reverse_doorbell(host_notifications: u64) {
    if host_notifications > 0 {
        // A future model-B carveout doorbell could deliver this; log it if seen.
        info!("[RT mailbox] RT->host doorbell observed ({host_notifications})");
    } else {
        info!("[RT mailbox] RT->host notification is poll-based on riscv64 (single SSWI line)");
    }
}

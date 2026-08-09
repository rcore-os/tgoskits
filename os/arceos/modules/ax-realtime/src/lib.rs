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
//! confined to the doorbell notification plane, so adding an architecture only
//! touches this crate, not the consuming kernels.

#![no_std]

use core::sync::atomic::Ordering;

use log::{info, warn};

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
    setup_rt_mailbox_doorbell(cpu_id);
    ax_rt::run_realtime_cpu(cpu_id, config.tasks, config.time_fn)
}

/// Arms interrupt-driven RT→host mailbox notification on the current host core.
///
/// Call this once from the host boot CPU, which is also the core that drains the
/// RT→host ring (`host_mailbox_recv`).
pub fn setup_host_side() {
    setup_host_mailbox_doorbell();
}

/// GIC SGI used for the host→RT mailbox doorbell. SGI 0 is the scheduler IPI,
/// so the mailbox uses a dedicated line the host runtime never targets.
#[cfg(target_arch = "aarch64")]
const MAILBOX_DOORBELL_SGI_TO_RT: u32 = 1;

#[cfg(target_arch = "aarch64")]
static RT_MAILBOX_CPU: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);

/// Resolves the GIC `IrqId` of the mailbox doorbell SGI at runtime.
///
/// The GIC IRQ domain id is assigned dynamically during boot, so the doorbell
/// must borrow the same domain the runtime IPI already uses. The
/// `AARCH64_GIC_DOMAIN` compatibility constant is not the registered id: the
/// platform's `is_gic_domain` check rejects it, which makes both
/// `request_percpu_irq` (registration) and `send_ipi` (delivery) fail with
/// `InvalidIrq` and silently fall back to polling.
#[cfg(target_arch = "aarch64")]
fn mailbox_doorbell_irq() -> ax_hal::irq::IrqId {
    use ax_hal::irq;
    let gic_domain = irq::ipi_irq().domain;
    irq::IrqId::new(gic_domain, irq::HwIrq(MAILBOX_DOORBELL_SGI_TO_RT))
}

/// Doorbell that rings the reserved RT core after a host→RT send.
#[cfg(target_arch = "aarch64")]
struct RtCoreDoorbell;

#[cfg(target_arch = "aarch64")]
impl ax_rt::MailboxDoorbell for RtCoreDoorbell {
    fn ring(&self) {
        use ax_hal::{irq, percpu};
        let cpu = RT_MAILBOX_CPU.load(Ordering::Acquire);
        if cpu == usize::MAX {
            return;
        }
        info!(
            "[RT mailbox] doorbell IPI: host CPU{} -> RT CPU{cpu} (SGI \
             {MAILBOX_DOORBELL_SGI_TO_RT})",
            percpu::this_cpu_id()
        );
        irq::send_ipi(
            mailbox_doorbell_irq(),
            irq::IpiTarget::Other { cpu_id: cpu },
        );
    }
}

#[cfg(target_arch = "aarch64")]
static RT_CORE_DOORBELL: RtCoreDoorbell = RtCoreDoorbell;

/// Enables interrupt-driven mailbox notification on the reserved RT core.
///
/// The RT core deliberately skips the ordinary secondary IRQ-online path, so it
/// enables only this one dedicated doorbell SGI here: the scheduler timer and
/// IPI stay registered on host CPUs only, keeping the RT core's interrupt
/// surface minimal. The handler runs in interrupt context and does nothing but
/// set the mailbox pending flag.
#[cfg(target_arch = "aarch64")]
fn setup_rt_mailbox_doorbell(cpu_id: usize) {
    use ax_hal::irq;

    RT_MAILBOX_CPU.store(cpu_id, Ordering::Release);
    irq::init_common_irq_handler();
    if let Err(err) = irq::cpu_online(cpu_id) {
        warn!("RT mailbox doorbell: cpu_online({cpu_id}) failed: {err:?}");
        return;
    }
    let doorbell_irq = mailbox_doorbell_irq();
    let doorbell_cpus = irq::CpuMask::from_cpu(irq::CpuId(cpu_id));
    let result = irq::request_percpu_irq(doorbell_irq, doorbell_cpus, |_ctx| {
        ax_rt::rt_mailbox_on_doorbell();
        irq::IrqReturn::Handled
    });
    if let Err(err) = result {
        warn!("RT mailbox doorbell: request_percpu_irq failed: {err:?}");
        return;
    }
    // From now on host_mailbox_send() rings this core instead of relying on the
    // RT task's fallback poll.
    ax_rt::set_rt_doorbell(&RT_CORE_DOORBELL);
    ax_hal::asm::enable_irqs();
    info!("RT mailbox doorbell armed on CPU {cpu_id} (SGI {MAILBOX_DOORBELL_SGI_TO_RT}).");
}

/// Non-aarch64 fallback: mailbox notification stays poll-based.
#[cfg(not(target_arch = "aarch64"))]
fn setup_rt_mailbox_doorbell(_cpu_id: usize) {}

/// GIC SGI used for the RT→host mailbox doorbell. SGI 0 is the scheduler IPI and
/// SGI 1 is the host→RT doorbell, so the reverse direction takes a third line.
#[cfg(target_arch = "aarch64")]
const MAILBOX_DOORBELL_SGI_TO_HOST: u32 = 2;

/// Host core that drains the RT→host ring, i.e. the target of the reverse
/// doorbell. Set once when the host arms its doorbell.
#[cfg(target_arch = "aarch64")]
static RT_MAILBOX_HOST_CPU: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);

/// Resolves the GIC `IrqId` of the RT→host mailbox doorbell SGI at runtime.
///
/// Uses the dynamically registered GIC domain for the same reason as
/// [`mailbox_doorbell_irq`].
#[cfg(target_arch = "aarch64")]
fn host_mailbox_doorbell_irq() -> ax_hal::irq::IrqId {
    use ax_hal::irq;
    let gic_domain = irq::ipi_irq().domain;
    irq::IrqId::new(gic_domain, irq::HwIrq(MAILBOX_DOORBELL_SGI_TO_HOST))
}

/// Doorbell that rings the host consumer core after an RT→host send.
#[cfg(target_arch = "aarch64")]
struct HostCoreDoorbell;

#[cfg(target_arch = "aarch64")]
impl ax_rt::MailboxDoorbell for HostCoreDoorbell {
    fn ring(&self) {
        use ax_hal::irq;
        let target = RT_MAILBOX_HOST_CPU.load(Ordering::Acquire);
        if target == usize::MAX {
            return;
        }
        // Runs on the isolated RT core: keep it to the raw SGI and do not touch
        // the shared console lock here. The host logs the reverse IPI when it
        // observes the doorbell, so both directions stay visible without the RT
        // core contending on host-owned logging state.
        irq::send_ipi(
            host_mailbox_doorbell_irq(),
            irq::IpiTarget::Other { cpu_id: target },
        );
    }
}

#[cfg(target_arch = "aarch64")]
static HOST_CORE_DOORBELL: HostCoreDoorbell = HostCoreDoorbell;

/// Arms interrupt-driven RT→host mailbox notification on the current host core.
///
/// Runs on the host boot CPU, which is also the core that drains the RT→host
/// ring (`host_mailbox_recv`) from the boot self-test and the shell. Registering
/// the reverse doorbell here lets the RT core signal the host with a real SGI
/// rather than relying on the host to poll, so a host→RT command and its RT→host
/// reply become a symmetric exchange of doorbell IPIs between the two cores.
#[cfg(target_arch = "aarch64")]
fn setup_host_mailbox_doorbell() {
    use ax_hal::{irq, percpu};

    // The host CPU is already online in the IRQ framework and running with IRQs
    // enabled, so unlike the RT core this path only registers the extra line.
    let cpu_id = percpu::this_cpu_id();
    RT_MAILBOX_HOST_CPU.store(cpu_id, Ordering::Release);
    let doorbell_irq = host_mailbox_doorbell_irq();
    let doorbell_cpus = irq::CpuMask::from_cpu(irq::CpuId(cpu_id));
    let result = irq::request_percpu_irq(doorbell_irq, doorbell_cpus, |_ctx| {
        ax_rt::host_mailbox_on_doorbell();
        irq::IrqReturn::Handled
    });
    if let Err(err) = result {
        warn!("host mailbox doorbell: request_percpu_irq failed: {err:?}");
        return;
    }
    ax_rt::set_host_doorbell(&HOST_CORE_DOORBELL);
    info!("Host mailbox doorbell armed on CPU {cpu_id} (SGI {MAILBOX_DOORBELL_SGI_TO_HOST}).");
}

/// Non-aarch64 fallback: RT→host notification stays poll-based.
#[cfg(not(target_arch = "aarch64"))]
fn setup_host_mailbox_doorbell() {}

/// Logs whether the host observed the RT core's reverse doorbell IPI.
///
/// Call from the host self-test after the round-trip completes. On aarch64 a
/// nonzero notification count means the RT core signalled the host with a real
/// SGI; zero means the reverse path silently fell back to polling.
#[cfg(target_arch = "aarch64")]
pub fn report_reverse_doorbell(host_notifications: u64) {
    use ax_hal::percpu;
    if host_notifications > 0 {
        info!(
            "[RT mailbox] doorbell IPI: RT core -> host CPU{} received (SGI \
             {MAILBOX_DOORBELL_SGI_TO_HOST})",
            percpu::this_cpu_id()
        );
    } else {
        warn!("[RT mailbox] reverse doorbell IPI not observed; RT->host fell back to polling");
    }
}

/// Non-aarch64 fallback: RT→host notification is poll-based, nothing to report.
#[cfg(not(target_arch = "aarch64"))]
pub fn report_reverse_doorbell(_host_notifications: u64) {}

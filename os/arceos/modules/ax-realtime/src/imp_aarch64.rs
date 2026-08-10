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

//! aarch64 doorbell backend: two dedicated GIC SGIs.
//!
//! One SGI per direction, both distinct from the scheduler IPI (SGI 0):
//!
//! | SGI | direction        | sender core → target core |
//! |-----|------------------|---------------------------|
//! | 1   | host → RT        | host core → reserved RT core |
//! | 2   | RT → host        | RT core → host consumer core |
//!
//! Each receive-side handler runs in interrupt context and only sets the mailbox
//! pending flag (`*_on_doorbell`); the drain happens on the owning core outside
//! the ISR. See the crate-level [`DoorbellArch`](crate::DoorbellArch) contract.

use core::sync::atomic::{AtomicUsize, Ordering};

use log::{info, warn};

use crate::DoorbellArch;

/// aarch64 GIC-SGI doorbell backend. Zero-sized; per-core state lives in this
/// module's statics.
pub struct Doorbell;

impl DoorbellArch for Doorbell {
    fn setup_rt_side(cpu_id: usize) {
        setup_rt_mailbox_doorbell(cpu_id);
    }

    fn setup_host_side() {
        setup_host_mailbox_doorbell();
    }

    fn report_reverse_doorbell(host_notifications: u64) {
        report_reverse_doorbell(host_notifications);
    }
}

/// GIC SGI used for the host→RT mailbox doorbell. SGI 0 is the scheduler IPI, so
/// the mailbox uses a dedicated line the host runtime never targets.
const MAILBOX_DOORBELL_SGI_TO_RT: u32 = 1;

/// GIC SGI used for the RT→host mailbox doorbell. SGI 0 is the scheduler IPI and
/// SGI 1 is the host→RT doorbell, so the reverse direction takes a third line.
const MAILBOX_DOORBELL_SGI_TO_HOST: u32 = 2;

static RT_MAILBOX_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Host core that drains the RT→host ring, i.e. the target of the reverse
/// doorbell. Set once when the host arms its doorbell.
static RT_MAILBOX_HOST_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Resolves the GIC `IrqId` of the host→RT mailbox doorbell SGI at runtime.
///
/// The GIC IRQ domain id is assigned dynamically during boot, so the doorbell
/// must borrow the same domain the runtime IPI already uses. The
/// `AARCH64_GIC_DOMAIN` compatibility constant is not the registered id: the
/// platform's `is_gic_domain` check rejects it, which makes both
/// `request_percpu_irq` (registration) and `send_ipi` (delivery) fail with
/// `InvalidIrq` and silently fall back to polling.
fn mailbox_doorbell_irq() -> ax_hal::irq::IrqId {
    use ax_hal::irq;
    let gic_domain = irq::ipi_irq().domain;
    irq::IrqId::new(gic_domain, irq::HwIrq(MAILBOX_DOORBELL_SGI_TO_RT))
}

/// Resolves the GIC `IrqId` of the RT→host mailbox doorbell SGI at runtime.
///
/// Uses the dynamically registered GIC domain for the same reason as
/// [`mailbox_doorbell_irq`].
fn host_mailbox_doorbell_irq() -> ax_hal::irq::IrqId {
    use ax_hal::irq;
    let gic_domain = irq::ipi_irq().domain;
    irq::IrqId::new(gic_domain, irq::HwIrq(MAILBOX_DOORBELL_SGI_TO_HOST))
}

/// Doorbell that rings the reserved RT core after a host→RT send.
struct RtCoreDoorbell;

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
        irq::send_ipi(mailbox_doorbell_irq(), irq::IpiTarget::Cpu(irq::CpuId(cpu)));
    }
}

static RT_CORE_DOORBELL: RtCoreDoorbell = RtCoreDoorbell;

/// Doorbell that rings the host consumer core after an RT→host send.
struct HostCoreDoorbell;

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
            irq::IpiTarget::Cpu(irq::CpuId(target)),
        );
    }
}

static HOST_CORE_DOORBELL: HostCoreDoorbell = HostCoreDoorbell;

/// Enables interrupt-driven mailbox notification on the reserved RT core.
///
/// The RT core deliberately skips the ordinary secondary IRQ-online path, so it
/// enables only this one dedicated doorbell SGI here: the scheduler timer and
/// IPI stay registered on host CPUs only, keeping the RT core's interrupt
/// surface minimal. The handler runs in interrupt context and does nothing but
/// set the mailbox pending flag.
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
    disable_rt_core_timer_irq();
    // From now on host_mailbox_send() rings this core instead of relying on the
    // RT task's fallback poll.
    ax_rt::set_rt_doorbell(&RT_CORE_DOORBELL);
    ax_hal::asm::enable_irqs();
    info!("RT mailbox doorbell armed on CPU {cpu_id} (SGI {MAILBOX_DOORBELL_SGI_TO_RT}).");
}

fn disable_rt_core_timer_irq() {
    use ax_hal::{irq, time};

    let timer_irq = time::irq_num();
    if timer_irq.hwirq.0 >= 32 {
        warn!("RT mailbox doorbell: timer IRQ {timer_irq:?} is not a private GIC interrupt");
        return;
    }

    // The reserved RT core does not register axruntime's scheduler timer action.
    // Keep the local arch timer PPI masked so enabling IRQs exposes only the
    // dedicated mailbox SGIs on this core.
    if let Err(err) = irq::set_enable(timer_irq, false) {
        warn!("RT mailbox doorbell: failed to disable timer IRQ {timer_irq:?}: {err:?}");
    }
}

/// Arms interrupt-driven RT→host mailbox notification on the current host core.
///
/// Runs on the host boot CPU, which is also the core that drains the RT→host
/// ring (`host_mailbox_recv`) from the boot self-test and the shell. Registering
/// the reverse doorbell here lets the RT core signal the host with a real SGI
/// rather than relying on the host to poll, so a host→RT command and its RT→host
/// reply become a symmetric exchange of doorbell IPIs between the two cores.
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

/// Logs whether the host observed the RT core's reverse doorbell IPI.
///
/// On aarch64 a nonzero notification count means the RT core signalled the host
/// with a real SGI; zero means the reverse path silently fell back to polling.
fn report_reverse_doorbell(host_notifications: u64) {
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

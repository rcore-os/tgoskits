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
    RtMessage, RtMutex, RtSemaphore, RtTask, rt_delay_until, rt_exit_current_task, rt_mailbox_recv,
    rt_mailbox_send, rt_output_write, rt_sleep,
};
pub use ax_rt::{
    RtState, RtTaskState, host_mailbox_recv, host_mailbox_send, rt_mailbox_stats, rt_read_output,
    status,
};

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
/// host→RT command tag: echo the payload back as an event.
const MAILBOX_CMD_ECHO: u32 = 0x01;
/// RT→host event tag reported by the echo task (command tag | 0x80).
const MAILBOX_EVT_ECHO: u32 = MAILBOX_CMD_ECHO | 0x80;
/// Payload the host round-trip test sends and expects echoed back.
const MAILBOX_TEST_PAYLOAD: &[u8] = b"rt-mailbox-ping";

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
static RT_MAILBOX_TEST_RESULT: AtomicU64 = AtomicU64::new(0);
static RT_TASKS: [RtTask; 11] = [
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
    RtTask::with_priority("mbox-echo", 0, 8, mailbox_echo_task),
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

    setup_rt_mailbox_doorbell(cpu_id);

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

/// Long-running RT service task: drains host→RT commands and echoes each one
/// back to the host as an RT→host event (`tag | 0x80`, same payload).
///
/// Consuming the mailbox pending flag makes the poll loop cheap once IPI-driven
/// notification is wired; today it also polls on a short period as a fallback.
fn mailbox_echo_task() -> ! {
    loop {
        let _woken = ax_rt::rt_mailbox_take_pending();
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

pub fn log_priority_test_result() {
    let mut priority_done = false;
    let mut recursive_done = false;
    let mut semaphore_done = false;
    let mut mailbox_done = false;
    let mailbox_ping =
        RtMessage::new(MAILBOX_CMD_ECHO, MAILBOX_TEST_PAYLOAD).expect("mailbox test payload fits");
    let mut mailbox_sent = host_mailbox_send(&mailbox_ping).is_ok();
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
                    let ipi_deadline_nanos = monotonic_time_nanos().saturating_add(10_000_000);
                    while stats.host_notifications == 0
                        && monotonic_time_nanos() < ipi_deadline_nanos
                    {
                        core::hint::spin_loop();
                        stats = rt_mailbox_stats();
                    }
                    // Report the reverse IPI from the host side (the RT core is
                    // the only sender of this doorbell SGI) so both directions
                    // are visible without logging from the isolated RT core.
                    report_reverse_doorbell(stats.host_notifications);
                    info!(
                        "[RT mailbox test] host->RT->host round-trip PASS (rt_notifications={}, host_notifications={})",
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
    if !mailbox_done {
        let stats = rt_mailbox_stats();
        warn!(
            "[RT mailbox test] timed out waiting for echo: sent={mailbox_sent}, \
             to_rt_depth={}, to_host_depth={}, to_rt_dropped={}, to_host_dropped={}",
            stats.to_rt_depth, stats.to_host_depth, stats.to_rt_dropped, stats.to_host_dropped
        );
    }
}

/// Shell helper: send `text` to the RT core as an echo command (host→RT).
pub fn mailbox_send_command(text: &[u8]) -> Result<(), ax_rt::RtMailboxError> {
    let msg = RtMessage::new(MAILBOX_CMD_ECHO, text)?;
    host_mailbox_send(&msg)
}

/// Shell helper: drain one RT→host event, copying its payload into `out`.
/// Returns `(tag, copied_len)`, or `None` when no event is queued.
pub fn mailbox_recv_into(out: &mut [u8]) -> Option<(u32, usize)> {
    let msg = host_mailbox_recv()?;
    let copied = msg.payload().len().min(out.len());
    out[..copied].copy_from_slice(&msg.payload()[..copied]);
    Some((msg.tag(), copied))
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
fn mailbox_doorbell_irq() -> ax_std::os::arceos::modules::ax_hal::irq::IrqId {
    use ax_std::os::arceos::modules::ax_hal::irq;
    let gic_domain = irq::ipi_irq().domain;
    irq::IrqId::new(gic_domain, irq::HwIrq(MAILBOX_DOORBELL_SGI_TO_RT))
}

/// Doorbell that rings the reserved RT core after a host→RT send.
#[cfg(target_arch = "aarch64")]
struct RtCoreDoorbell;

#[cfg(target_arch = "aarch64")]
impl ax_rt::MailboxDoorbell for RtCoreDoorbell {
    fn ring(&self) {
        use ax_std::os::arceos::modules::ax_hal::{irq, percpu};
        let cpu = RT_MAILBOX_CPU.load(Ordering::Acquire);
        if cpu == usize::MAX {
            return;
        }
        info!(
            "[RT mailbox] doorbell IPI: host CPU{} -> RT CPU{cpu} (SGI {MAILBOX_DOORBELL_SGI_TO_RT})",
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
    use ax_std::os::arceos::modules::ax_hal;

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
fn host_mailbox_doorbell_irq() -> ax_std::os::arceos::modules::ax_hal::irq::IrqId {
    use ax_std::os::arceos::modules::ax_hal::irq;
    let gic_domain = irq::ipi_irq().domain;
    irq::IrqId::new(gic_domain, irq::HwIrq(MAILBOX_DOORBELL_SGI_TO_HOST))
}

/// Doorbell that rings the host consumer core after an RT→host send.
#[cfg(target_arch = "aarch64")]
struct HostCoreDoorbell;

#[cfg(target_arch = "aarch64")]
impl ax_rt::MailboxDoorbell for HostCoreDoorbell {
    fn ring(&self) {
        use ax_std::os::arceos::modules::ax_hal::irq;
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
pub fn setup_host_mailbox_doorbell() {
    use ax_std::os::arceos::modules::ax_hal::{irq, percpu};

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
pub fn setup_host_mailbox_doorbell() {}

/// Logs whether the host observed the RT core's reverse doorbell IPI.
///
/// Called from the host self-test after the round-trip completes. On aarch64 a
/// nonzero notification count means the RT core signalled the host with a real
/// SGI; zero means the reverse path silently fell back to polling.
#[cfg(target_arch = "aarch64")]
fn report_reverse_doorbell(host_notifications: u64) {
    use ax_std::os::arceos::modules::ax_hal::percpu;
    if host_notifications > 0 {
        info!(
            "[RT mailbox] doorbell IPI: RT core -> host CPU{} received (SGI {MAILBOX_DOORBELL_SGI_TO_HOST})",
            percpu::this_cpu_id()
        );
    } else {
        warn!("[RT mailbox] reverse doorbell IPI not observed; RT->host fell back to polling");
    }
}

/// Non-aarch64 fallback: RT→host notification is poll-based, nothing to report.
#[cfg(not(target_arch = "aarch64"))]
fn report_reverse_doorbell(_host_notifications: u64) {}

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

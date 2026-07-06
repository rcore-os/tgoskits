//! Deterministic host virtual SMP and IRQ runtime for tests.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{
    BareCpuCore, BareTaskRuntime, CpuId, CpuMask, FifoScheduler, HardIrqWaker, IpiEvent, TaskCore,
    TaskId, TaskRef, TaskState, WakeResult,
};

/// Virtual IRQ identifier.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct IrqId(pub usize);

/// Virtual IRQ trigger mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqMode {
    /// Pending edge events coalesce while the line is pending.
    Edge,
    /// A level line remains pending until explicitly acknowledged.
    Level,
}

/// Virtual CPU register state observable by tests.
#[derive(Clone, Debug)]
pub struct VirtualRegisters {
    /// CPU represented by this host worker.
    pub cpu_id: CpuId,
    /// Current task on this CPU.
    pub current_task: Option<TaskId>,
    /// Local IRQ enable flag.
    pub irq_enabled: bool,
    /// Whether this CPU is executing a hard IRQ callback.
    pub in_irq: bool,
    /// Preemption disable depth.
    pub preempt_depth: usize,
    /// Whether scheduling should be re-evaluated after IRQ exit.
    pub need_resched: bool,
}

impl VirtualRegisters {
    fn new(cpu_id: CpuId) -> Self {
        Self {
            cpu_id,
            current_task: None,
            irq_enabled: true,
            in_irq: false,
            preempt_depth: 0,
            need_resched: false,
        }
    }
}

/// Virtual CPU state.
pub struct HostCpu {
    /// Virtual registers.
    pub regs: VirtualRegisters,
    core: Arc<BareCpuCore<FifoScheduler<TaskRef>>>,
    delivered_ipis: usize,
    delivered_irqs: usize,
}

impl HostCpu {
    fn new(cpu_id: CpuId, core: Arc<BareCpuCore<FifoScheduler<TaskRef>>>) -> Self {
        Self {
            regs: VirtualRegisters::new(cpu_id),
            core,
            delivered_ipis: 0,
            delivered_irqs: 0,
        }
    }

    /// Returns the number of runnable tasks queued on this CPU.
    pub fn run_queue_len(&self) -> usize {
        self.core.run_queue_len()
    }

    /// Returns the number of delivered virtual IRQ callbacks.
    pub fn delivered_irqs(&self) -> usize {
        self.delivered_irqs
    }

    /// Returns the number of delivered virtual IPI events.
    pub fn delivered_ipis(&self) -> usize {
        self.delivered_ipis
    }
}

type IrqCallback = Box<dyn FnMut(&mut HostSmpRuntime, CpuId, IrqId) + Send>;

struct IrqLine {
    enabled: bool,
    pending: bool,
    mode: IrqMode,
    affinity: CpuMask,
    callback: Option<IrqCallback>,
}

struct TimerEvent {
    id: u64,
    cpu: CpuId,
    deadline: u64,
    task: TaskRef,
    active: bool,
}

/// Deterministic virtual SMP runtime.
pub struct HostSmpRuntime {
    task_runtime: BareTaskRuntime<FifoScheduler<TaskRef>>,
    cpus: Vec<HostCpu>,
    irqs: Vec<IrqLine>,
    now_nanos: u64,
    next_task_id: AtomicU64,
    next_timer_id: u64,
    timers: Vec<TimerEvent>,
    ipi_irq: IrqId,
    timer_irq: IrqId,
}

impl HostSmpRuntime {
    /// Creates a virtual runtime with `cpu_count` CPUs.
    pub fn new(cpu_count: usize) -> Self {
        assert!(cpu_count > 0);
        let task_runtime = BareTaskRuntime::new();
        let cpus = (0..cpu_count)
            .map(|cpu| {
                let cpu_id = CpuId(cpu);
                let core = task_runtime.add_cpu(cpu_id, FifoScheduler::new());
                HostCpu::new(cpu_id, core)
            })
            .collect();
        let mut runtime = Self {
            task_runtime,
            cpus,
            irqs: Vec::new(),
            now_nanos: 0,
            next_task_id: AtomicU64::new(1),
            next_timer_id: 1,
            timers: Vec::new(),
            ipi_irq: IrqId(usize::MAX),
            timer_irq: IrqId(usize::MAX),
        };
        runtime.ipi_irq =
            runtime.register_irq(IrqMode::Edge, CpuMask::first(cpu_count), |rt, cpu, _| {
                rt.drain_ipi_queue(cpu);
            });
        runtime.timer_irq =
            runtime.register_irq(IrqMode::Edge, CpuMask::first(cpu_count), |rt, cpu, _| {
                rt.drain_expired_timers(cpu);
            });
        runtime
    }

    /// Returns an immutable CPU reference.
    pub fn cpu(&self, cpu: CpuId) -> &HostCpu {
        &self.cpus[cpu.0]
    }

    /// Returns a mutable CPU reference.
    pub fn cpu_mut(&mut self, cpu: CpuId) -> &mut HostCpu {
        &mut self.cpus[cpu.0]
    }

    /// Creates a task assigned to `cpu`.
    pub fn create_task(&self, cpu: CpuId) -> TaskRef {
        Arc::new(TaskCore::new(
            TaskId(self.next_task_id.fetch_add(1, Ordering::AcqRel)),
            cpu,
        ))
    }

    /// Registers a virtual IRQ line.
    pub fn register_irq(
        &mut self,
        mode: IrqMode,
        affinity: CpuMask,
        callback: impl FnMut(&mut HostSmpRuntime, CpuId, IrqId) + Send + 'static,
    ) -> IrqId {
        let id = IrqId(self.irqs.len());
        self.irqs.push(IrqLine {
            enabled: true,
            pending: false,
            mode,
            affinity,
            callback: Some(Box::new(callback)),
        });
        id
    }

    /// Enables or disables a virtual IRQ line.
    pub fn set_irq_enabled(&mut self, irq: IrqId, enabled: bool) {
        self.irqs[irq.0].enabled = enabled;
    }

    /// Changes virtual IRQ affinity.
    pub fn set_irq_affinity(&mut self, irq: IrqId, affinity: CpuMask) {
        self.irqs[irq.0].affinity = affinity;
    }

    /// Raises a virtual IRQ line.
    pub fn raise_irq(&mut self, irq: IrqId) {
        self.irqs[irq.0].pending = true;
    }

    /// Acknowledges a virtual IRQ line.
    pub fn ack_irq(&mut self, irq: IrqId) {
        self.irqs[irq.0].pending = false;
    }

    /// Delivers one pending IRQ to `cpu`.
    pub fn deliver_irq(&mut self, cpu: CpuId) -> bool {
        let Some(irq) = self.next_deliverable_irq(cpu) else {
            return false;
        };
        let mode = self.irqs[irq.0].mode;
        if mode == IrqMode::Edge {
            self.irqs[irq.0].pending = false;
        }
        let mut callback = self.irqs[irq.0]
            .callback
            .take()
            .expect("registered IRQ callback missing");
        self.cpus[cpu.0].regs.in_irq = true;
        callback(self, cpu, irq);
        self.cpus[cpu.0].regs.in_irq = false;
        self.cpus[cpu.0].delivered_irqs += 1;
        self.irqs[irq.0].callback = Some(callback);
        self.irq_epilogue(cpu);
        true
    }

    /// Delivers at most one pending IRQ per CPU.
    pub fn deliver_all_irqs(&mut self) -> usize {
        let mut delivered = 0;
        for cpu in 0..self.cpus.len() {
            if self.deliver_irq(CpuId(cpu)) {
                delivered += 1;
            }
        }
        delivered
    }

    fn next_deliverable_irq(&self, cpu: CpuId) -> Option<IrqId> {
        if !self.cpus[cpu.0].regs.irq_enabled {
            return None;
        }
        self.irqs
            .iter()
            .enumerate()
            .find(|(_, line)| line.enabled && line.pending && line.affinity.contains(cpu))
            .map(|(id, _)| IrqId(id))
    }

    /// Sends a virtual IPI event to a CPU.
    pub fn send_ipi_reschedule(&mut self, cpu: CpuId) {
        if self.cpus[cpu.0].core.request_ipi(IpiEvent::Reschedule) {
            self.raise_irq(self.ipi_irq);
        }
    }

    /// Sends a virtual IRQ-wake IPI event to a CPU.
    pub fn send_ipi_irq_wake(&mut self, cpu: CpuId) {
        if self.cpus[cpu.0].core.request_ipi(IpiEvent::IrqWakeDrain) {
            self.raise_irq(self.ipi_irq);
        }
    }

    fn drain_ipi_queue(&mut self, cpu: CpuId) {
        let events = self.task_runtime.on_ipi_irq(cpu);
        self.cpus[cpu.0].delivered_ipis += events.count();
        if events.contains(IpiEvent::Reschedule) {
            self.cpus[cpu.0].regs.need_resched = true;
        }
    }

    /// Publishes a hard IRQ wake and inserts the task into its target CPU queue.
    pub fn wake_from_irq(&mut self, waker: &HardIrqWaker, bits: u64) -> WakeResult {
        let result = self.task_runtime.wake_from_irq(waker, bits);
        if result.woke() {
            self.raise_irq(self.ipi_irq);
        }
        result
    }

    /// Drains one CPU's hard IRQ wake queue.
    pub fn drain_irq_wake_queue(&mut self, cpu: CpuId) -> usize {
        self.task_runtime.drain_irq_wake_queue(cpu)
    }

    /// Runs the virtual IRQ epilogue on `cpu`.
    pub fn irq_epilogue(&mut self, cpu: CpuId) {
        self.drain_irq_wake_queue(cpu);
    }

    /// Schedules a virtual timer that wakes `task`.
    pub fn schedule_timer(&mut self, cpu: CpuId, deadline: u64, task: TaskRef) -> u64 {
        let id = self.next_timer_id;
        self.next_timer_id += 1;
        self.timers.push(TimerEvent {
            id,
            cpu,
            deadline,
            task,
            active: true,
        });
        id
    }

    /// Cancels a virtual timer.
    pub fn cancel_timer(&mut self, id: u64) {
        if let Some(timer) = self.timers.iter_mut().find(|timer| timer.id == id) {
            timer.active = false;
        }
    }

    /// Advances virtual monotonic time.
    pub fn advance_time(&mut self, nanos: u64) {
        self.now_nanos = self.now_nanos.saturating_add(nanos);
        let due_cpus: Vec<CpuId> = self
            .timers
            .iter()
            .filter(|timer| timer.active && timer.deadline <= self.now_nanos)
            .map(|timer| timer.cpu)
            .collect();
        for cpu in due_cpus {
            self.raise_irq(self.timer_irq);
            self.set_irq_affinity(self.timer_irq, CpuMask::one(cpu));
        }
    }

    fn drain_expired_timers(&mut self, cpu: CpuId) {
        let mut expired = Vec::new();
        for timer in &mut self.timers {
            if timer.active && timer.cpu == cpu && timer.deadline <= self.now_nanos {
                timer.active = false;
                expired.push(timer.task.clone());
            }
        }
        for task in expired {
            task.set_state(TaskState::Ready);
            self.task_runtime.add_ready_task(cpu, task);
            self.cpus[cpu.0].regs.need_resched = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::{TaskWaker, WakeSeq};

    #[test]
    fn hard_irq_callback_defers_scheduler_work_until_epilogue() {
        let observed_blocked = Arc::new(AtomicBool::new(false));
        let mut runtime = HostSmpRuntime::new(1);
        let task = runtime.create_task(CpuId(0));
        task.set_state(TaskState::Blocked);
        let waker = TaskWaker::new(task.clone()).to_hard_irq_waker();
        let observed = observed_blocked.clone();

        let irq = runtime.register_irq(IrqMode::Edge, CpuMask::one(CpuId(0)), move |rt, _, _| {
            observed.store(task.state() == TaskState::Blocked, Ordering::Release);
            rt.wake_from_irq(&waker, 0x1);
        });
        runtime.raise_irq(irq);

        assert!(runtime.deliver_irq(CpuId(0)));
        assert!(observed_blocked.load(Ordering::Acquire));
        assert_eq!(runtime.cpu(CpuId(0)).run_queue_len(), 1);
    }

    #[test]
    fn disabled_irq_stays_pending_until_enabled() {
        let hits = Arc::new(AtomicUsize::new(0));
        let mut runtime = HostSmpRuntime::new(1);
        let hits_for_irq = hits.clone();
        let irq = runtime.register_irq(IrqMode::Edge, CpuMask::one(CpuId(0)), move |_, _, _| {
            hits_for_irq.fetch_add(1, Ordering::AcqRel);
        });

        runtime.set_irq_enabled(irq, false);
        runtime.raise_irq(irq);
        assert!(!runtime.deliver_irq(CpuId(0)));
        assert_eq!(hits.load(Ordering::Acquire), 0);

        runtime.set_irq_enabled(irq, true);
        assert!(runtime.deliver_irq(CpuId(0)));
        assert_eq!(hits.load(Ordering::Acquire), 1);
    }

    #[test]
    fn edge_irq_coalesces_multiple_raises_before_delivery() {
        let hits = Arc::new(AtomicUsize::new(0));
        let mut runtime = HostSmpRuntime::new(1);
        let hits_for_irq = hits.clone();
        let irq = runtime.register_irq(IrqMode::Edge, CpuMask::one(CpuId(0)), move |_, _, _| {
            hits_for_irq.fetch_add(1, Ordering::AcqRel);
        });

        runtime.raise_irq(irq);
        runtime.raise_irq(irq);
        assert!(runtime.deliver_irq(CpuId(0)));
        assert!(!runtime.deliver_irq(CpuId(0)));
        assert_eq!(hits.load(Ordering::Acquire), 1);
    }

    #[test]
    fn level_irq_remains_pending_without_spinning() {
        let hits = Arc::new(AtomicUsize::new(0));
        let mut runtime = HostSmpRuntime::new(1);
        let hits_for_irq = hits.clone();
        let irq = runtime.register_irq(IrqMode::Level, CpuMask::one(CpuId(0)), move |_, _, _| {
            hits_for_irq.fetch_add(1, Ordering::AcqRel);
        });

        runtime.raise_irq(irq);
        assert!(runtime.deliver_irq(CpuId(0)));
        assert_eq!(hits.load(Ordering::Acquire), 1);
        assert!(runtime.deliver_irq(CpuId(0)));
        assert_eq!(hits.load(Ordering::Acquire), 2);
        runtime.ack_irq(irq);
        assert!(!runtime.deliver_irq(CpuId(0)));
    }

    #[test]
    fn irq_affinity_controls_delivery_cpu() {
        let mut runtime = HostSmpRuntime::new(2);
        let irq = runtime.register_irq(IrqMode::Edge, CpuMask::one(CpuId(1)), |_, _, _| {});

        runtime.raise_irq(irq);
        assert!(!runtime.deliver_irq(CpuId(0)));
        assert!(runtime.deliver_irq(CpuId(1)));
        assert_eq!(runtime.cpu(CpuId(1)).delivered_irqs(), 1);
    }

    #[test]
    fn ipi_is_delivered_through_virtual_irq() {
        let mut runtime = HostSmpRuntime::new(2);

        runtime.send_ipi_reschedule(CpuId(1));
        assert!(!runtime.cpu(CpuId(1)).regs.need_resched);
        assert!(runtime.deliver_irq(CpuId(1)));
        assert!(runtime.cpu(CpuId(1)).regs.need_resched);
        assert_eq!(runtime.cpu(CpuId(1)).delivered_ipis(), 1);
    }

    #[test]
    fn remote_hard_irq_wake_drains_on_target_cpu() {
        let mut runtime = HostSmpRuntime::new(2);
        let task = runtime.create_task(CpuId(1));
        task.set_state(TaskState::Blocked);
        let waker = TaskWaker::new(task.clone()).to_hard_irq_waker();

        let result = runtime.wake_from_irq(&waker, 0x55);

        assert!(result.woke());
        assert_eq!(task.state(), TaskState::Blocked);
        assert_eq!(runtime.drain_irq_wake_queue(CpuId(1)), 1);
        assert_eq!(task.state(), TaskState::Ready);
        assert_eq!(waker.take_bits(), 0x55);
    }

    #[test]
    fn concurrent_wake_metadata_coalesces_once() {
        let mut runtime = HostSmpRuntime::new(1);
        let task = runtime.create_task(CpuId(0));
        task.set_state(TaskState::Blocked);
        let waker = TaskWaker::new(task.clone()).to_hard_irq_waker();
        let initial_seq: WakeSeq = waker.seq();

        let first = runtime.wake_from_irq(&waker, 0x1);
        let second = runtime.wake_from_irq(&waker, 0x2);

        assert!(first.woke());
        assert!(!second.woke());
        assert_eq!(waker.seq(), initial_seq + 2);
        assert_eq!(waker.take_bits(), 0x3);
        assert_eq!(runtime.drain_irq_wake_queue(CpuId(0)), 1);
    }

    #[test]
    fn expired_generation_waker_is_ignored() {
        let mut runtime = HostSmpRuntime::new(1);
        let task = runtime.create_task(CpuId(0));
        task.set_state(TaskState::Blocked);
        let waker = TaskWaker::new(task.clone()).to_hard_irq_waker();

        task.expire_wakers();
        let result = runtime.wake_from_irq(&waker, 0x1);

        assert!(!result.woke());
        assert_eq!(runtime.drain_irq_wake_queue(CpuId(0)), 0);
    }

    #[test]
    fn virtual_timer_irq_wakes_due_task_and_cancel_skips_stale_timer() {
        let mut runtime = HostSmpRuntime::new(1);
        let stale = runtime.create_task(CpuId(0));
        let due = runtime.create_task(CpuId(0));
        stale.set_state(TaskState::Blocked);
        due.set_state(TaskState::Blocked);

        let stale_timer = runtime.schedule_timer(CpuId(0), 10, stale.clone());
        runtime.schedule_timer(CpuId(0), 10, due.clone());
        runtime.cancel_timer(stale_timer);
        runtime.advance_time(10);
        assert!(runtime.deliver_irq(CpuId(0)));

        assert_eq!(stale.state(), TaskState::Blocked);
        assert_eq!(due.state(), TaskState::Ready);
        assert_eq!(runtime.cpu(CpuId(0)).run_queue_len(), 1);
    }
}

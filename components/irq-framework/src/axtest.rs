use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axtest::prelude::*;

use crate::{
    AutoEnable, CpuId, CpuMask, HwIrq, IrqAffinity, IrqDomainId, IrqError, IrqExecution, IrqId,
    IrqOps, IrqRequest, IrqReturn, IrqScope, Registry, ShareMode,
};

const TEST_DOMAIN: IrqDomainId = IrqDomainId(7);
const ALL_CPUS: usize = 0b1111;

fn irq(line: u32) -> IrqId {
    IrqId::new(TEST_DOMAIN, HwIrq(line))
}

fn count_request(counter: &'static AtomicUsize) -> IrqRequest {
    IrqRequest::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        IrqReturn::Handled
    })
}

struct MockOps {
    current_cpu: AtomicUsize,
    online_bits: AtomicUsize,
    in_irq: AtomicBool,
    unsupported_status: AtomicBool,
    remote_calls: AtomicUsize,
    set_enabled_calls: AtomicUsize,
    set_affinity_calls: AtomicUsize,
    global_enabled: AtomicUsize,
    percpu_enabled: [AtomicUsize; 4],
}

impl MockOps {
    fn new() -> Self {
        Self {
            current_cpu: AtomicUsize::new(0),
            online_bits: AtomicUsize::new(ALL_CPUS),
            in_irq: AtomicBool::new(false),
            unsupported_status: AtomicBool::new(false),
            remote_calls: AtomicUsize::new(0),
            set_enabled_calls: AtomicUsize::new(0),
            set_affinity_calls: AtomicUsize::new(0),
            global_enabled: AtomicUsize::new(usize::MAX),
            percpu_enabled: [
                AtomicUsize::new(usize::MAX),
                AtomicUsize::new(usize::MAX),
                AtomicUsize::new(usize::MAX),
                AtomicUsize::new(usize::MAX),
            ],
        }
    }

    fn set_current_cpu(&self, cpu: usize) {
        self.current_cpu.store(cpu, Ordering::SeqCst);
    }

    fn set_online(&self, cpu: usize, online: bool) {
        let mask = 1usize << cpu;
        if online {
            self.online_bits.fetch_or(mask, Ordering::SeqCst);
        } else {
            self.online_bits.fetch_and(!mask, Ordering::SeqCst);
        }
    }

    fn set_unsupported_status(&self, unsupported: bool) {
        self.unsupported_status.store(unsupported, Ordering::SeqCst);
    }

    fn set_in_irq(&self, in_irq: bool) {
        self.in_irq.store(in_irq, Ordering::SeqCst);
    }

    fn line_enabled(&self, irq: IrqId, cpu: Option<CpuId>) -> bool {
        let bit = 1usize << irq.hwirq.0;
        match cpu {
            Some(cpu) => self.percpu_enabled[cpu.0].load(Ordering::SeqCst) & bit != 0,
            None => self.global_enabled.load(Ordering::SeqCst) & bit != 0,
        }
    }

    fn store_line_enabled(&self, irq: IrqId, cpu: Option<CpuId>, enabled: bool) {
        let bit = 1usize << irq.hwirq.0;
        let target = match cpu {
            Some(cpu) => &self.percpu_enabled[cpu.0],
            None => &self.global_enabled,
        };
        if enabled {
            target.fetch_or(bit, Ordering::SeqCst);
        } else {
            target.fetch_and(!bit, Ordering::SeqCst);
        }
    }
}

impl IrqOps for &'static MockOps {
    type LocalIrqState = ();

    fn current_cpu(&self) -> CpuId {
        CpuId(self.current_cpu.load(Ordering::SeqCst))
    }

    fn cpu_online(&self, cpu: CpuId) -> bool {
        self.online_bits.load(Ordering::SeqCst) & (1usize << cpu.0) != 0
    }

    fn in_irq_context(&self) -> bool {
        self.in_irq.load(Ordering::SeqCst)
    }

    fn local_irq_save(&self) -> Self::LocalIrqState {}

    fn local_irq_restore(&self, _state: Self::LocalIrqState) {}

    fn run_on_cpu_sync(
        &self,
        cpu: CpuId,
        f: unsafe fn(*mut ()),
        arg: *mut (),
    ) -> Result<(), IrqError> {
        self.remote_calls.fetch_add(1, Ordering::SeqCst);
        let old_cpu = self.current_cpu();
        self.set_current_cpu(cpu.0);
        unsafe { f(arg) };
        self.set_current_cpu(old_cpu.0);
        Ok(())
    }

    fn set_affinity(&self, _irq: IrqId, _affinity: IrqAffinity) -> Result<(), IrqError> {
        self.set_affinity_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn set_enabled(&self, irq: IrqId, cpu: Option<CpuId>, enabled: bool) -> Result<(), IrqError> {
        self.set_enabled_calls.fetch_add(1, Ordering::SeqCst);
        self.store_line_enabled(irq, cpu, enabled);
        Ok(())
    }

    fn is_enabled(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<bool, IrqError> {
        if self.unsupported_status.load(Ordering::SeqCst) {
            return Err(IrqError::Unsupported);
        }
        Ok(self.line_enabled(irq, cpu))
    }

    fn is_pending(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        if self.unsupported_status.load(Ordering::SeqCst) {
            return Err(IrqError::Unsupported);
        }
        Ok(false)
    }

    fn is_in_service(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        if self.unsupported_status.load(Ordering::SeqCst) {
            return Err(IrqError::Unsupported);
        }
        Ok(false)
    }

    fn relax(&self) {
        core::hint::spin_loop();
    }
}

fn leaked_ops() -> &'static MockOps {
    alloc::boxed::Box::leak(alloc::boxed::Box::new(MockOps::new()))
}

#[axtest]
fn irq_framework_request_dispatch_status_and_free_global_action() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    CALLS.store(0, Ordering::SeqCst);

    let ops = leaked_ops();
    let registry = Registry::new(ops);
    let handle = registry.request(irq(3), count_request(&CALLS)).unwrap();

    ax_assert_eq!(handle.irq(), irq(3));
    ax_assert_eq!(handle.id(), 1);
    ax_assert_eq!(ops.set_enabled_calls.load(Ordering::SeqCst), 2);
    ax_assert!(registry.status(handle).unwrap().action_enabled);

    let outcome = registry.dispatch(irq(3), CpuId(0));
    ax_assert!(outcome.handled);
    ax_assert!(!outcome.wake);
    ax_assert_eq!(outcome.called, 1);
    ax_assert_eq!(CALLS.load(Ordering::SeqCst), 1);

    registry.disable(handle).unwrap();
    ax_assert!(!registry.status(handle).unwrap().action_enabled);
    ax_assert_eq!(registry.dispatch(irq(3), CpuId(0)).called, 0);
    registry.enable(handle).unwrap();
    ax_assert_eq!(registry.dispatch(irq(3), CpuId(0)).called, 1);

    registry.free(handle).unwrap();
    ax_assert_eq!(registry.dispatch(irq(3), CpuId(0)).called, 0);
    ax_assert_eq!(registry.free(handle), Err(IrqError::NotFound));
}

#[axtest]
fn irq_framework_shared_requests_dispatch_all_actions_and_reject_exclusive_peer() {
    static FIRST: AtomicUsize = AtomicUsize::new(0);
    static SECOND: AtomicUsize = AtomicUsize::new(0);
    FIRST.store(0, Ordering::SeqCst);
    SECOND.store(0, Ordering::SeqCst);

    let registry = Registry::new(leaked_ops());
    registry
        .request(irq(4), count_request(&FIRST).share_mode(ShareMode::Shared))
        .unwrap();
    registry
        .request(
            irq(4),
            IrqRequest::new(|_| {
                SECOND.fetch_add(1, Ordering::SeqCst);
                IrqReturn::Wake
            })
            .share_mode(ShareMode::Shared),
        )
        .unwrap();

    let outcome = registry.dispatch(irq(4), CpuId(0));
    ax_assert!(outcome.handled);
    ax_assert!(outcome.wake);
    ax_assert_eq!(outcome.called, 2);
    ax_assert_eq!(FIRST.load(Ordering::SeqCst), 1);
    ax_assert_eq!(SECOND.load(Ordering::SeqCst), 1);

    let err = registry
        .request(irq(4), IrqRequest::new(|_| IrqReturn::Handled))
        .unwrap_err();
    ax_assert_eq!(err, IrqError::Busy);
}

#[axtest]
fn irq_framework_concurrent_request_dispatches_with_wake_outcome() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    CALLS.store(0, Ordering::SeqCst);

    let registry = Registry::new(leaked_ops());
    registry
        .request(
            irq(8),
            IrqRequest::new_concurrent(|_| {
                CALLS.fetch_add(1, Ordering::SeqCst);
                IrqReturn::Wake
            })
            .execution(IrqExecution::Concurrent),
        )
        .unwrap();

    let outcome = registry.dispatch(irq(8), CpuId(0));
    ax_assert!(outcome.handled);
    ax_assert!(outcome.wake);
    ax_assert_eq!(outcome.called, 1);
    ax_assert_eq!(CALLS.load(Ordering::SeqCst), 1);
}

#[axtest]
fn irq_framework_percpu_request_tracks_offline_pending_enable() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    CALLS.store(0, Ordering::SeqCst);

    let ops = leaked_ops();
    ops.set_current_cpu(0);
    ops.set_online(2, false);
    let registry = Registry::new(ops);
    let handle = registry
        .request(
            irq(5),
            count_request(&CALLS)
                .scope(IrqScope::PerCpu {
                    cpus: CpuMask::from_cpu(CpuId(2)),
                })
                .auto_enable(AutoEnable::No),
        )
        .unwrap();

    ax_assert_eq!(registry.dispatch(irq(5), CpuId(2)).called, 0);
    registry.enable(handle).unwrap();
    ax_assert_eq!(ops.remote_calls.load(Ordering::SeqCst), 0);
    ops.set_online(2, true);
    registry.cpu_online(CpuId(2)).unwrap();
    ax_assert!(registry.status(handle).unwrap().line_enabled);
    ax_assert_eq!(registry.dispatch(irq(5), CpuId(2)).called, 1);
    ax_assert_eq!(registry.dispatch(irq(5), CpuId(1)).called, 0);
}

#[axtest]
fn irq_framework_rejects_invalid_contexts_and_requests() {
    let ops = leaked_ops();
    let registry = Registry::new(ops);

    let mut empty = CpuMask::empty();
    ax_assert!(empty.is_empty());
    let err = registry
        .request(
            irq(6),
            IrqRequest::new(|_| IrqReturn::Handled).scope(IrqScope::PerCpu { cpus: empty }),
        )
        .unwrap_err();
    ax_assert_eq!(err, IrqError::InvalidCpu);

    empty.insert(CpuId(1));
    ops.set_online(1, false);
    let err = registry
        .request(
            irq(6),
            IrqRequest::new(|_| IrqReturn::Handled).affinity(IrqAffinity::Fixed(CpuId(1))),
        )
        .unwrap_err();
    ax_assert_eq!(err, IrqError::CpuOffline);

    let handle = registry
        .request(irq(7), IrqRequest::new(|_| IrqReturn::Unhandled))
        .unwrap();
    ops.set_in_irq(true);
    ax_assert_eq!(registry.free(handle), Err(IrqError::InIrqContext));
    ops.set_in_irq(false);

    ops.set_unsupported_status(true);
    let status = registry.status(handle).unwrap();
    ax_assert!(status.line_enabled);
    ax_assert!(!status.pending);
    ax_assert!(!status.in_service);
}

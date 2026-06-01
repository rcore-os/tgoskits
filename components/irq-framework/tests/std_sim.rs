use std::{
    ptr::NonNull,
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

use irq_framework::{
    AutoEnable, CpuId, CpuMask, IrqContext, IrqError, IrqNumber, IrqOps, IrqRequest, IrqReturn,
    IrqScope, Registry, ShareMode,
};

#[derive(Clone, Default)]
struct MockOps {
    inner: Arc<MockInner>,
}

#[derive(Default)]
struct MockInner {
    current_cpu: AtomicUsize,
    in_irq: AtomicBool,
    unsupported_status: AtomicBool,
    online: Mutex<Vec<bool>>,
    calls: Mutex<Vec<OpCall>>,
    fail_set_enabled: Mutex<Vec<(usize, Option<usize>, bool)>>,
    remote_calls: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OpCall {
    SetEnabled {
        irq: usize,
        cpu: Option<usize>,
        enabled: bool,
    },
    IsEnabled {
        irq: usize,
        cpu: Option<usize>,
    },
    IsPending {
        irq: usize,
        cpu: Option<usize>,
    },
    IsInService {
        irq: usize,
        cpu: Option<usize>,
    },
}

impl MockOps {
    fn with_cpus(count: usize) -> Self {
        Self {
            inner: Arc::new(MockInner {
                online: Mutex::new(vec![true; count]),
                ..MockInner::default()
            }),
        }
    }

    fn set_current_cpu(&self, cpu: usize) {
        self.inner.current_cpu.store(cpu, Ordering::SeqCst);
    }

    fn set_online(&self, cpu: usize, online: bool) {
        self.inner.online.lock().unwrap()[cpu] = online;
    }

    fn set_in_irq(&self, in_irq: bool) {
        self.inner.in_irq.store(in_irq, Ordering::SeqCst);
    }

    fn set_unsupported_status(&self, unsupported: bool) {
        self.inner
            .unsupported_status
            .store(unsupported, Ordering::SeqCst);
    }

    fn fail_set_enabled(&self, irq: usize, cpu: Option<usize>, enabled: bool) {
        self.inner
            .fail_set_enabled
            .lock()
            .unwrap()
            .push((irq, cpu, enabled));
    }

    fn calls(&self) -> Vec<OpCall> {
        self.inner.calls.lock().unwrap().clone()
    }
}

impl IrqOps for MockOps {
    type LocalIrqState = ();

    fn current_cpu(&self) -> CpuId {
        CpuId(self.inner.current_cpu.load(Ordering::SeqCst))
    }

    fn cpu_online(&self, cpu: CpuId) -> bool {
        self.inner
            .online
            .lock()
            .unwrap()
            .get(cpu.0)
            .copied()
            .unwrap_or(false)
    }

    fn in_irq_context(&self) -> bool {
        self.inner.in_irq.load(Ordering::SeqCst)
    }

    fn local_irq_save(&self) -> Self::LocalIrqState {}

    fn local_irq_restore(&self, _state: Self::LocalIrqState) {}

    fn run_on_cpu_sync(
        &self,
        cpu: CpuId,
        f: unsafe fn(*mut ()),
        arg: *mut (),
    ) -> Result<(), IrqError> {
        self.inner.remote_calls.fetch_add(1, Ordering::SeqCst);
        let old = self.current_cpu();
        self.set_current_cpu(cpu.0);
        unsafe { f(arg) };
        self.set_current_cpu(old.0);
        Ok(())
    }

    fn set_enabled(
        &self,
        irq: IrqNumber,
        cpu: Option<CpuId>,
        enabled: bool,
    ) -> Result<(), IrqError> {
        self.inner.calls.lock().unwrap().push(OpCall::SetEnabled {
            irq: irq.0,
            cpu: cpu.map(|cpu| cpu.0),
            enabled,
        });
        if self.inner.fail_set_enabled.lock().unwrap().contains(&(
            irq.0,
            cpu.map(|cpu| cpu.0),
            enabled,
        )) {
            return Err(IrqError::Controller);
        }
        Ok(())
    }

    fn is_enabled(&self, irq: IrqNumber, cpu: Option<CpuId>) -> Result<bool, IrqError> {
        self.inner.calls.lock().unwrap().push(OpCall::IsEnabled {
            irq: irq.0,
            cpu: cpu.map(|cpu| cpu.0),
        });
        if self.inner.unsupported_status.load(Ordering::SeqCst) {
            return Err(IrqError::Unsupported);
        }
        Ok(true)
    }

    fn is_pending(&self, irq: IrqNumber, cpu: Option<CpuId>) -> Result<bool, IrqError> {
        self.inner.calls.lock().unwrap().push(OpCall::IsPending {
            irq: irq.0,
            cpu: cpu.map(|cpu| cpu.0),
        });
        if self.inner.unsupported_status.load(Ordering::SeqCst) {
            return Err(IrqError::Unsupported);
        }
        Ok(false)
    }

    fn is_in_service(&self, irq: IrqNumber, cpu: Option<CpuId>) -> Result<bool, IrqError> {
        self.inner.calls.lock().unwrap().push(OpCall::IsInService {
            irq: irq.0,
            cpu: cpu.map(|cpu| cpu.0),
        });
        if self.inner.unsupported_status.load(Ordering::SeqCst) {
            return Err(IrqError::Unsupported);
        }
        Ok(false)
    }

    fn relax(&self) {
        thread::yield_now();
    }
}

unsafe fn count_handler(ctx: IrqContext, data: NonNull<()>) -> IrqReturn {
    assert!(ctx.irq.0 > 0);
    let counter = unsafe { data.cast::<AtomicUsize>().as_ref() };
    counter.fetch_add(1, Ordering::SeqCst);
    IrqReturn::Handled
}

unsafe fn wake_handler(_ctx: IrqContext, data: NonNull<()>) -> IrqReturn {
    let counter = unsafe { data.cast::<AtomicUsize>().as_ref() };
    counter.fetch_add(1, Ordering::SeqCst);
    IrqReturn::Wake
}

#[test]
fn dynamic_shared_actions_all_dispatch() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops);
    let mut counters = Vec::new();

    for _ in 0..64 {
        counters.push(Box::new(AtomicUsize::new(0)));
        let data = NonNull::from(counters.last().unwrap().as_ref()).cast();
        registry
            .request(
                IrqNumber(7),
                IrqRequest::new(count_handler, data).share_mode(ShareMode::Shared),
            )
            .unwrap();
    }

    let outcome = registry.dispatch(IrqNumber(7), CpuId(0));
    assert!(outcome.handled);
    assert!(!outcome.wake);
    assert_eq!(outcome.called, 64);
    assert!(
        counters
            .iter()
            .all(|counter| counter.load(Ordering::SeqCst) == 1)
    );
}

#[test]
fn exclusive_and_shared_conflict() {
    let registry = Registry::new(MockOps::with_cpus(1));
    let counter = AtomicUsize::new(0);
    let data = NonNull::from(&counter).cast();

    registry
        .request(
            IrqNumber(3),
            IrqRequest::new(count_handler, data).auto_enable(AutoEnable::No),
        )
        .unwrap();

    let err = registry
        .request(
            IrqNumber(3),
            IrqRequest::new(count_handler, data)
                .share_mode(ShareMode::Shared)
                .auto_enable(AutoEnable::No),
        )
        .unwrap_err();

    assert_eq!(err, IrqError::Busy);
}

#[test]
fn free_waits_for_inflight_dispatch_and_detaches_action() {
    struct Blocker {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
        calls: AtomicUsize,
    }

    unsafe fn blocking_handler(_ctx: IrqContext, data: NonNull<()>) -> IrqReturn {
        let blocker = unsafe { data.cast::<Blocker>().as_ref() };
        blocker.calls.fetch_add(1, Ordering::SeqCst);
        blocker.entered.wait();
        blocker.release.wait();
        IrqReturn::Handled
    }

    let registry = Arc::new(Registry::new(MockOps::with_cpus(1)));
    let blocker = Box::new(Blocker {
        entered: Arc::new(Barrier::new(2)),
        release: Arc::new(Barrier::new(2)),
        calls: AtomicUsize::new(0),
    });
    let data = NonNull::from(blocker.as_ref()).cast();
    let handle = registry
        .request(IrqNumber(11), IrqRequest::new(blocking_handler, data))
        .unwrap();

    let dispatch_registry = registry.clone();
    let dispatch_thread =
        thread::spawn(move || dispatch_registry.dispatch(IrqNumber(11), CpuId(0)));

    blocker.entered.wait();

    let free_registry = registry.clone();
    let free_thread = thread::spawn(move || free_registry.free(handle));

    thread::sleep(std::time::Duration::from_millis(30));
    assert!(!free_thread.is_finished());

    blocker.release.wait();
    assert!(dispatch_thread.join().unwrap().handled);
    free_thread.join().unwrap().unwrap();

    let outcome = registry.dispatch(IrqNumber(11), CpuId(0));
    assert!(!outcome.handled);
    assert_eq!(outcome.called, 0);
    assert_eq!(blocker.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn per_cpu_action_dispatches_only_on_matching_cpu() {
    let registry = Registry::new(MockOps::with_cpus(4));
    let counter = AtomicUsize::new(0);
    let data = NonNull::from(&counter).cast();
    let cpus = CpuMask::from_cpu(CpuId(2));

    registry
        .request(
            IrqNumber(9),
            IrqRequest::new(count_handler, data).scope(IrqScope::PerCpu { cpus }),
        )
        .unwrap();

    assert_eq!(registry.dispatch(IrqNumber(9), CpuId(0)).called, 0);
    assert_eq!(registry.dispatch(IrqNumber(9), CpuId(2)).called, 1);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn remote_per_cpu_enable_uses_run_on_cpu_sync() {
    let ops = MockOps::with_cpus(4);
    ops.set_current_cpu(0);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);
    let data = NonNull::from(&counter).cast();

    let handle = registry
        .request(
            IrqNumber(12),
            IrqRequest::new(count_handler, data)
                .scope(IrqScope::PerCpu {
                    cpus: CpuMask::from_cpu(CpuId(2)),
                })
                .auto_enable(AutoEnable::No),
        )
        .unwrap();

    registry.enable(handle).unwrap();

    assert_eq!(ops.inner.remote_calls.load(Ordering::SeqCst), 1);
    assert!(ops.calls().contains(&OpCall::SetEnabled {
        irq: 12,
        cpu: Some(2),
        enabled: true,
    }));
}

#[test]
fn failed_per_cpu_enable_rolls_back_action_state() {
    let ops = MockOps::with_cpus(4);
    ops.set_current_cpu(0);
    ops.fail_set_enabled(18, Some(2), true);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);
    let data = NonNull::from(&counter).cast();

    let handle = registry
        .request(
            IrqNumber(18),
            IrqRequest::new(count_handler, data)
                .scope(IrqScope::PerCpu {
                    cpus: CpuMask::from_cpu(CpuId(2)),
                })
                .auto_enable(AutoEnable::No),
        )
        .unwrap();

    assert_eq!(registry.enable(handle), Err(IrqError::Controller));
    assert_eq!(registry.dispatch(IrqNumber(18), CpuId(2)).called, 0);
    ops.set_unsupported_status(true);
    let status = registry.status(handle).unwrap();
    assert!(!status.action_enabled);
    assert!(!status.line_enabled);
}

#[test]
fn offline_cpu_enable_is_applied_when_cpu_comes_online() {
    let ops = MockOps::with_cpus(4);
    ops.set_current_cpu(0);
    ops.set_online(3, false);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);
    let data = NonNull::from(&counter).cast();

    let handle = registry
        .request(
            IrqNumber(13),
            IrqRequest::new(count_handler, data)
                .scope(IrqScope::PerCpu {
                    cpus: CpuMask::from_cpu(CpuId(3)),
                })
                .auto_enable(AutoEnable::No),
        )
        .unwrap();

    registry.enable(handle).unwrap();
    assert!(!ops.calls().contains(&OpCall::SetEnabled {
        irq: 13,
        cpu: Some(3),
        enabled: true,
    }));

    ops.set_online(3, true);
    registry.cpu_online(CpuId(3)).unwrap();

    assert!(ops.calls().contains(&OpCall::SetEnabled {
        irq: 13,
        cpu: Some(3),
        enabled: true,
    }));
}

#[test]
fn pending_enable_is_tracked_per_cpu() {
    let ops = MockOps::with_cpus(4);
    ops.set_current_cpu(0);
    ops.set_online(2, false);
    ops.set_online(3, false);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);
    let data = NonNull::from(&counter).cast();
    let mut cpus = CpuMask::empty();
    cpus.insert(CpuId(2));
    cpus.insert(CpuId(3));

    let handle = registry
        .request(
            IrqNumber(19),
            IrqRequest::new(count_handler, data)
                .scope(IrqScope::PerCpu { cpus })
                .auto_enable(AutoEnable::No),
        )
        .unwrap();

    registry.enable(handle).unwrap();
    assert!(!ops.calls().contains(&OpCall::SetEnabled {
        irq: 19,
        cpu: Some(2),
        enabled: true,
    }));
    assert!(!ops.calls().contains(&OpCall::SetEnabled {
        irq: 19,
        cpu: Some(3),
        enabled: true,
    }));

    ops.set_online(2, true);
    registry.cpu_online(CpuId(2)).unwrap();
    assert!(ops.calls().contains(&OpCall::SetEnabled {
        irq: 19,
        cpu: Some(2),
        enabled: true,
    }));
    assert!(!ops.calls().contains(&OpCall::SetEnabled {
        irq: 19,
        cpu: Some(3),
        enabled: true,
    }));

    ops.set_online(3, true);
    registry.cpu_online(CpuId(3)).unwrap();
    assert!(ops.calls().contains(&OpCall::SetEnabled {
        irq: 19,
        cpu: Some(3),
        enabled: true,
    }));
}

#[test]
fn freeing_per_cpu_action_disables_target_cpu_line() {
    let ops = MockOps::with_cpus(2);
    ops.set_current_cpu(0);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);
    let data = NonNull::from(&counter).cast();

    let handle = registry
        .request(
            IrqNumber(17),
            IrqRequest::new(count_handler, data)
                .scope(IrqScope::PerCpu {
                    cpus: CpuMask::from_cpu(CpuId(0)),
                })
                .auto_enable(AutoEnable::No),
        )
        .unwrap();

    registry.enable(handle).unwrap();
    registry.free(handle).unwrap();

    assert!(ops.calls().contains(&OpCall::SetEnabled {
        irq: 17,
        cpu: Some(0),
        enabled: false,
    }));
}

#[test]
fn status_queries_controller_state() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);
    let data = NonNull::from(&counter).cast();

    let handle = registry
        .request(
            IrqNumber(14),
            IrqRequest::new(count_handler, data).auto_enable(AutoEnable::No),
        )
        .unwrap();

    let status = registry.status(handle).unwrap();
    assert!(!status.action_enabled);
    assert!(status.line_enabled);
    assert!(!status.pending);
    assert!(!status.in_service);
    assert_eq!(status.in_flight, 0);
    assert!(
        ops.calls()
            .contains(&OpCall::IsEnabled { irq: 14, cpu: None })
    );
    assert!(
        ops.calls()
            .contains(&OpCall::IsPending { irq: 14, cpu: None })
    );
    assert!(
        ops.calls()
            .contains(&OpCall::IsInService { irq: 14, cpu: None })
    );
}

#[test]
fn status_uses_framework_line_state_when_controller_status_is_unsupported() {
    let ops = MockOps::with_cpus(1);
    ops.set_unsupported_status(true);
    let registry = Registry::new(ops);
    let counter = AtomicUsize::new(0);
    let data = NonNull::from(&counter).cast();

    let handle = registry
        .request(
            IrqNumber(20),
            IrqRequest::new(count_handler, data).auto_enable(AutoEnable::No),
        )
        .unwrap();

    let status = registry.status(handle).unwrap();
    assert!(!status.action_enabled);
    assert!(!status.line_enabled);
    assert!(!status.pending);
    assert!(!status.in_service);

    registry.enable(handle).unwrap();
    let status = registry.status(handle).unwrap();
    assert!(status.action_enabled);
    assert!(status.line_enabled);
    assert!(!status.pending);
    assert!(!status.in_service);
}

#[derive(Clone)]
struct BlockingLineOps {
    inner: Arc<BlockingLineInner>,
}

struct BlockingLineInner {
    false_entered: Barrier,
    false_release: Barrier,
    block_false_once: AtomicBool,
    line_enabled: AtomicBool,
    calls: Mutex<Vec<OpCall>>,
}

impl BlockingLineOps {
    fn new() -> Self {
        Self {
            inner: Arc::new(BlockingLineInner {
                false_entered: Barrier::new(2),
                false_release: Barrier::new(2),
                block_false_once: AtomicBool::new(true),
                line_enabled: AtomicBool::new(false),
                calls: Mutex::new(Vec::new()),
            }),
        }
    }
}

impl IrqOps for BlockingLineOps {
    type LocalIrqState = ();

    fn current_cpu(&self) -> CpuId {
        CpuId(0)
    }

    fn cpu_online(&self, cpu: CpuId) -> bool {
        cpu.0 == 0
    }

    fn in_irq_context(&self) -> bool {
        false
    }

    fn local_irq_save(&self) -> Self::LocalIrqState {}

    fn local_irq_restore(&self, _state: Self::LocalIrqState) {}

    fn run_on_cpu_sync(
        &self,
        _cpu: CpuId,
        _f: unsafe fn(*mut ()),
        _arg: *mut (),
    ) -> Result<(), IrqError> {
        unreachable!("test only uses the current CPU")
    }

    fn set_enabled(
        &self,
        irq: IrqNumber,
        cpu: Option<CpuId>,
        enabled: bool,
    ) -> Result<(), IrqError> {
        self.inner.calls.lock().unwrap().push(OpCall::SetEnabled {
            irq: irq.0,
            cpu: cpu.map(|cpu| cpu.0),
            enabled,
        });
        if !enabled && self.inner.block_false_once.swap(false, Ordering::SeqCst) {
            self.inner.false_entered.wait();
            self.inner.false_release.wait();
        }
        self.inner.line_enabled.store(enabled, Ordering::SeqCst);
        Ok(())
    }

    fn is_enabled(&self, _irq: IrqNumber, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn is_pending(&self, _irq: IrqNumber, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn is_in_service(&self, _irq: IrqNumber, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn relax(&self) {
        thread::yield_now();
    }
}

#[test]
fn stale_disable_does_not_override_concurrent_enable() {
    let ops = BlockingLineOps::new();
    let registry = Arc::new(Registry::new(ops.clone()));
    let first = AtomicUsize::new(0);
    let second = AtomicUsize::new(0);

    let first = registry
        .request(
            IrqNumber(21),
            IrqRequest::new(count_handler, NonNull::from(&first).cast())
                .share_mode(ShareMode::Shared),
        )
        .unwrap();
    let second = registry
        .request(
            IrqNumber(21),
            IrqRequest::new(count_handler, NonNull::from(&second).cast())
                .share_mode(ShareMode::Shared)
                .auto_enable(AutoEnable::No),
        )
        .unwrap();

    let disable_registry = registry.clone();
    let disable_thread = thread::spawn(move || disable_registry.disable(first));
    ops.inner.false_entered.wait();

    registry.enable(second).unwrap();
    ops.inner.false_release.wait();
    disable_thread.join().unwrap().unwrap();

    assert!(ops.inner.line_enabled.load(Ordering::SeqCst));
}

#[test]
fn disabling_one_shared_action_keeps_line_enabled_until_last_action() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let first = AtomicUsize::new(0);
    let second = AtomicUsize::new(0);

    let first = registry
        .request(
            IrqNumber(16),
            IrqRequest::new(count_handler, NonNull::from(&first).cast())
                .share_mode(ShareMode::Shared),
        )
        .unwrap();
    let second = registry
        .request(
            IrqNumber(16),
            IrqRequest::new(count_handler, NonNull::from(&second).cast())
                .share_mode(ShareMode::Shared),
        )
        .unwrap();

    registry.disable(first).unwrap();
    assert!(!ops.calls().contains(&OpCall::SetEnabled {
        irq: 16,
        cpu: None,
        enabled: false,
    }));

    registry.disable(second).unwrap();
    assert!(ops.calls().contains(&OpCall::SetEnabled {
        irq: 16,
        cpu: None,
        enabled: false,
    }));
}

#[test]
fn handler_can_report_wake_outcome() {
    let registry = Registry::new(MockOps::with_cpus(1));
    let counter = AtomicUsize::new(0);
    let data = NonNull::from(&counter).cast();

    registry
        .request(
            IrqNumber(15),
            IrqRequest::new(wake_handler, data).share_mode(ShareMode::Shared),
        )
        .unwrap();

    let outcome = registry.dispatch(IrqNumber(15), CpuId(0));
    assert!(outcome.handled);
    assert!(outcome.wake);
    assert_eq!(outcome.called, 1);
}

#[test]
fn free_from_irq_context_is_rejected() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);
    let data = NonNull::from(&counter).cast();
    let handle = registry
        .request(
            IrqNumber(16),
            IrqRequest::new(count_handler, data).auto_enable(AutoEnable::No),
        )
        .unwrap();

    ops.set_in_irq(true);
    assert_eq!(registry.free(handle), Err(IrqError::InIrqContext));
    ops.set_in_irq(false);
    registry.free(handle).unwrap();
}

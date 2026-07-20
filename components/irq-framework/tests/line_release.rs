use std::{
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
};

use irq_framework::{
    AutoEnable, CpuId, HwIrq, IrqAffinity, IrqDomainId, IrqError, IrqId, IrqLineBinding,
    IrqLineControl, IrqOps, IrqRequest, IrqReturn, IrqScope, PreparedIrqLine, Registry, ShareMode,
};

const TEST_DOMAIN: IrqDomainId = IrqDomainId(77);

fn irq(hwirq: u32) -> IrqId {
    IrqId::new(TEST_DOMAIN, HwIrq(hwirq))
}

#[derive(Clone, Default)]
struct ReleaseOps {
    state: Arc<ReleaseOpsState>,
}

#[derive(Default)]
struct ReleaseOpsState {
    next_generation: AtomicU64,
    active_binding: Mutex<Option<IrqLineBinding>>,
    prepare_calls: AtomicUsize,
    release_calls: AtomicUsize,
    fail_prepare: AtomicBool,
    fail_release: AtomicBool,
    release_blocker: Mutex<Option<Arc<ReleaseBlocker>>>,
}

struct ReleaseBlocker {
    entered: Barrier,
    release: Barrier,
}

impl ReleaseOps {
    fn fail_release(&self, fail: bool) {
        self.state.fail_release.store(fail, Ordering::SeqCst);
    }

    fn fail_prepare(&self, fail: bool) {
        self.state.fail_prepare.store(fail, Ordering::SeqCst);
    }

    fn block_next_release(&self) -> Arc<ReleaseBlocker> {
        let blocker = Arc::new(ReleaseBlocker {
            entered: Barrier::new(2),
            release: Barrier::new(2),
        });
        *self.state.release_blocker.lock().unwrap() = Some(Arc::clone(&blocker));
        blocker
    }
}

// SAFETY: this test adapter executes CPU callbacks synchronously and protects
// every shared controller-model field with atomics or a mutex.
unsafe impl IrqOps for ReleaseOps {
    type LocalIrqState = ();

    fn current_cpu(&self) -> CpuId {
        CpuId(0)
    }

    fn cpu_online(&self, cpu: CpuId) -> bool {
        cpu == CpuId(0)
    }

    fn in_irq_context(&self) -> bool {
        false
    }

    fn local_irq_save(&self) -> Self::LocalIrqState {}

    fn local_irq_restore(&self, _state: Self::LocalIrqState) {}

    fn run_on_cpu_sync(
        &self,
        _cpu: CpuId,
        callback: unsafe fn(*mut ()),
        argument: *mut (),
    ) -> Result<(), IrqError> {
        unsafe { callback(argument) };
        Ok(())
    }

    fn prepare_line(
        &self,
        irq: IrqId,
        _scope: IrqScope,
        _affinity: IrqAffinity,
    ) -> Result<PreparedIrqLine, IrqError> {
        self.state.prepare_calls.fetch_add(1, Ordering::SeqCst);
        if self.state.fail_prepare.load(Ordering::SeqCst) {
            return Err(IrqError::Controller);
        }
        let generation = self
            .state
            .next_generation
            .fetch_add(1, Ordering::SeqCst)
            .checked_add(1)
            .expect("test generation exhausted");
        let binding = IrqLineBinding::new(irq.hwirq.0, generation).unwrap();
        *self.state.active_binding.lock().unwrap() = Some(binding);
        Ok(PreparedIrqLine::new(binding, IrqLineControl::Maskable))
    }

    fn set_line_enabled(&self, binding: IrqLineBinding, _cpu: Option<CpuId>, _enabled: bool) {
        assert_eq!(*self.state.active_binding.lock().unwrap(), Some(binding));
    }

    fn release_line(&self, binding: IrqLineBinding) -> Result<(), IrqError> {
        self.state.release_calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(*self.state.active_binding.lock().unwrap(), Some(binding));
        let blocker = self.state.release_blocker.lock().unwrap().take();
        if let Some(blocker) = blocker {
            blocker.entered.wait();
            blocker.release.wait();
        }
        if self.state.fail_release.load(Ordering::SeqCst) {
            return Err(IrqError::Controller);
        }
        *self.state.active_binding.lock().unwrap() = None;
        Ok(())
    }

    fn relax(&self) {
        thread::yield_now();
    }
}

#[test]
fn sole_disabled_shared_action_releases_and_reprepares_its_line() {
    let ops = ReleaseOps::default();
    let registry = Registry::new(ops.clone());
    let irq = irq(8);
    let original = registry
        .request(
            irq,
            IrqRequest::new(|_| IrqReturn::Handled)
                .share_mode(ShareMode::Shared)
                .auto_enable(AutoEnable::No),
        )
        .unwrap();

    let (detached, released) = registry.detach_action_and_release_line(original).unwrap();

    assert_eq!(released.irq(), irq);
    assert_eq!(released.released_binding().generation(), 1);
    assert_eq!(registry.status(original), Err(IrqError::NotFound));
    assert_eq!(ops.state.release_calls.load(Ordering::SeqCst), 1);

    let reattached = registry.reattach_action(detached).unwrap();
    assert!(!registry.status(reattached).unwrap().action_enabled);
    assert_eq!(ops.state.prepare_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        ops.state
            .active_binding
            .lock()
            .unwrap()
            .unwrap()
            .generation(),
        2
    );
}

#[test]
fn failed_platform_release_rolls_back_to_the_usable_old_binding() {
    let ops = ReleaseOps::default();
    let registry = Registry::new(ops.clone());
    let irq = irq(9);
    let handle = registry
        .request(irq, IrqRequest::new(|_| IrqReturn::Handled))
        .unwrap();
    let old_binding = ops.state.active_binding.lock().unwrap().unwrap();
    ops.fail_release(true);

    assert!(matches!(
        registry.detach_action_and_release_line(handle),
        Err(IrqError::Controller)
    ));

    assert!(!registry.status(handle).unwrap().action_enabled);
    assert_eq!(*ops.state.active_binding.lock().unwrap(), Some(old_binding));
    ops.fail_release(false);
    registry.enable(handle).unwrap();
    assert!(registry.status(handle).unwrap().line_enabled);
}

#[test]
fn failed_reprepare_returns_unique_detached_action_for_retry() {
    let ops = ReleaseOps::default();
    let registry = Registry::new(ops.clone());
    let irq = irq(13);
    let handle = registry
        .request(irq, IrqRequest::new(|_| IrqReturn::Handled))
        .unwrap();
    let (detached, _released) = registry.detach_action_and_release_line(handle).unwrap();
    ops.fail_prepare(true);

    let error = registry.reattach_action(detached).unwrap_err();

    assert_eq!(error.reason(), IrqError::Controller);
    assert_eq!(*ops.state.active_binding.lock().unwrap(), None);
    let detached = error.into_action();
    ops.fail_prepare(false);
    let reattached = registry.reattach_action(detached).unwrap();
    assert!(!registry.status(reattached).unwrap().action_enabled);
}

#[test]
fn release_reservation_rejects_a_racing_registration_without_side_effects() {
    let ops = ReleaseOps::default();
    let registry = Arc::new(Registry::new(ops.clone()));
    let irq = irq(10);
    let handle = registry
        .request(
            irq,
            IrqRequest::new(|_| IrqReturn::Handled)
                .share_mode(ShareMode::Shared)
                .auto_enable(AutoEnable::No),
        )
        .unwrap();
    let blocker = ops.block_next_release();
    let releasing_registry = Arc::clone(&registry);
    let releasing =
        thread::spawn(move || releasing_registry.detach_action_and_release_line(handle));
    blocker.entered.wait();

    let peer = registry.request(
        irq,
        IrqRequest::new(|_| IrqReturn::Handled)
            .share_mode(ShareMode::Shared)
            .auto_enable(AutoEnable::No),
    );
    assert_eq!(peer, Err(IrqError::Busy));

    blocker.release.wait();
    let (_detached, _released) = releasing.join().unwrap().unwrap();
}

#[test]
fn shared_peer_prevents_line_release_without_detaching_the_selected_action() {
    let ops = ReleaseOps::default();
    let registry = Registry::new(ops.clone());
    let irq = irq(11);
    let request = || {
        IrqRequest::new(|_| IrqReturn::Handled)
            .share_mode(ShareMode::Shared)
            .auto_enable(AutoEnable::No)
    };
    let selected = registry.request(irq, request()).unwrap();
    let peer = registry.request(irq, request()).unwrap();

    assert!(matches!(
        registry.detach_action_and_release_line(selected),
        Err(IrqError::Busy)
    ));
    assert_eq!(ops.state.release_calls.load(Ordering::SeqCst), 0);
    assert!(registry.status(selected).is_ok());
    assert!(registry.status(peer).is_ok());
}

#[test]
fn controller_claim_must_finish_eoi_before_line_release() {
    let ops = ReleaseOps::default();
    let registry = Arc::new(Registry::new(ops.clone()));
    let irq = irq(12);
    let handle = registry
        .request(
            irq,
            IrqRequest::new(|_| IrqReturn::DisableActionAndWake).auto_enable(AutoEnable::Yes),
        )
        .unwrap();
    let eoi_entered = Arc::new(Barrier::new(2));
    let eoi_resume = Arc::new(Barrier::new(2));
    let dispatch_registry = Arc::clone(&registry);
    let dispatch_entered = Arc::clone(&eoi_entered);
    let dispatch_resume = Arc::clone(&eoi_resume);
    let dispatch = thread::spawn(move || {
        dispatch_registry.dispatch(irq, CpuId(0), || {
            dispatch_entered.wait();
            dispatch_resume.wait();
        })
    });
    eoi_entered.wait();

    assert!(matches!(
        registry.detach_action_and_release_line(handle),
        Err(IrqError::Busy)
    ));
    assert_eq!(ops.state.release_calls.load(Ordering::SeqCst), 0);
    assert!(!registry.status(handle).unwrap().action_enabled);

    eoi_resume.wait();
    assert!(dispatch.join().unwrap().handled);
    let (_detached, _released) = registry.detach_action_and_release_line(handle).unwrap();
    assert_eq!(ops.state.release_calls.load(Ordering::SeqCst), 1);
}

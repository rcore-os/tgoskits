use std::{
    cell::UnsafeCell,
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use irq_framework::{
    AutoEnable, CpuId, CpuMask, HwIrq, IrqAffinity, IrqContinuationToken, IrqContinuationWake,
    IrqDomainId, IrqDrainWake, IrqError, IrqExecution, IrqId, IrqOps, IrqRequest, IrqReturn,
    IrqScope, Registry, ShareMode,
};

const TEST_DOMAIN: IrqDomainId = IrqDomainId(1);
const TEST_DOMAIN_A: IrqDomainId = IrqDomainId(2);
const TEST_DOMAIN_B: IrqDomainId = IrqDomainId(3);

fn domain_irq(domain: IrqDomainId, hwirq: u32) -> IrqId {
    IrqId::new(domain, HwIrq(hwirq))
}

fn raw_irq(irq: IrqId) -> usize {
    irq.hwirq.0 as usize
}

fn irq(raw: usize) -> IrqId {
    let hwirq = u32::try_from(raw).expect("test IRQ number exceeds hwirq width");
    domain_irq(TEST_DOMAIN, hwirq)
}

fn enabled_request(
    handler: impl FnMut(irq_framework::IrqContext) -> IrqReturn + Send + 'static,
) -> IrqRequest {
    IrqRequest::new(handler).auto_enable(AutoEnable::Yes)
}

fn enabled_concurrent_request(
    handler: impl Fn(irq_framework::IrqContext) -> IrqReturn + Send + Sync + 'static,
) -> IrqRequest {
    IrqRequest::new_concurrent(handler).auto_enable(AutoEnable::Yes)
}

fn count_request(counter: &AtomicUsize) -> IrqRequest {
    let counter = counter as *const AtomicUsize as usize;
    enabled_request(move |ctx| {
        assert!(ctx.irq.hwirq.0 > 0);
        let counter = unsafe { &*(counter as *const AtomicUsize) };
        counter.fetch_add(1, Ordering::SeqCst);
        IrqReturn::Handled
    })
}

fn wake_request(counter: &AtomicUsize) -> IrqRequest {
    let counter = counter as *const AtomicUsize as usize;
    enabled_request(move |_| {
        let counter = unsafe { &*(counter as *const AtomicUsize) };
        counter.fetch_add(1, Ordering::SeqCst);
        IrqReturn::Wake
    })
}

struct ContinuationCapture {
    token: UnsafeCell<Option<IrqContinuationToken>>,
    ready: AtomicBool,
    wakes: AtomicUsize,
}

// The IRQ callback is the sole producer and publishes with Release. The test
// thread is the sole consumer and takes the slot only after an Acquire claim.
unsafe impl Sync for ContinuationCapture {}

impl ContinuationCapture {
    fn allocate() -> (&'static Self, &'static IrqContinuationWake) {
        let capture = Box::leak(Box::new(Self {
            token: UnsafeCell::new(None),
            ready: AtomicBool::new(false),
            wakes: AtomicUsize::new(0),
        }));
        let data = core::ptr::from_ref(capture).expose_provenance();
        let wake = Box::leak(Box::new(unsafe {
            // SAFETY: both capture and wake are leaked for the test process,
            // and the callback performs only fixed mutex/atomic publication.
            IrqContinuationWake::new(data, capture_continuation)
        }));
        (capture, wake)
    }

    fn take(&self) -> IrqContinuationToken {
        assert!(
            self.ready.swap(false, Ordering::AcqRel),
            "deferred dispatch must publish one continuation token"
        );
        unsafe {
            // SAFETY: the Acquire claim above makes this sole consumer own the
            // slot until the next producer publication.
            (*self.token.get())
                .take()
                .expect("published continuation slot must contain its token")
        }
    }
}

unsafe fn capture_continuation(data: usize, token: IrqContinuationToken) {
    let capture = unsafe {
        // SAFETY: `ContinuationCapture::allocate` publishes the address of a
        // leaked object and this callback is bound only to that address.
        &*core::ptr::with_exposed_provenance::<ContinuationCapture>(data)
    };
    assert!(
        !capture.ready.load(Ordering::Acquire),
        "one action cannot publish two live tokens"
    );
    unsafe {
        // SAFETY: line masking permits only this producer until the consumer
        // finishes the matching token.
        *capture.token.get() = Some(token);
    }
    capture.ready.store(true, Ordering::Release);
    capture.wakes.fetch_add(1, Ordering::SeqCst);
}

#[derive(Clone, Default)]
struct MockOps {
    inner: Arc<MockInner>,
}

#[derive(Default)]
struct MockInner {
    current_cpu: AtomicUsize,
    current_cpu_snapshots: AtomicUsize,
    in_irq: AtomicBool,
    unsupported_status: AtomicBool,
    online: Mutex<Vec<bool>>,
    line_enabled: Mutex<Vec<(usize, Option<usize>, bool)>>,
    calls: Mutex<Vec<OpCall>>,
    fail_set_enabled: Mutex<Vec<(usize, Option<usize>, bool)>>,
    fail_set_affinity: AtomicBool,
    affinity_blocker: Mutex<Option<Arc<AffinityBlocker>>>,
    cpu_sync_calls: AtomicUsize,
    local_irq_depth: AtomicUsize,
}

struct AffinityBlocker {
    entered: Barrier,
    release: Barrier,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OpCall {
    SetEnabled {
        irq: usize,
        cpu: Option<usize>,
        enabled: bool,
    },
    SetAffinity {
        irq: usize,
        affinity: IrqAffinity,
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

    fn fail_set_affinity(&self) {
        self.inner.fail_set_affinity.store(true, Ordering::SeqCst);
    }

    fn set_fail_set_affinity(&self, fail: bool) {
        self.inner.fail_set_affinity.store(fail, Ordering::SeqCst);
    }

    fn block_next_affinity(&self) -> Arc<AffinityBlocker> {
        let blocker = Arc::new(AffinityBlocker {
            entered: Barrier::new(2),
            release: Barrier::new(2),
        });
        *self.inner.affinity_blocker.lock().unwrap() = Some(Arc::clone(&blocker));
        blocker
    }

    fn set_line_enabled(&self, irq: usize, cpu: Option<usize>, enabled: bool) {
        let mut states = self.inner.line_enabled.lock().unwrap();
        if let Some((_, _, state)) = states
            .iter_mut()
            .find(|(entry_irq, entry_cpu, _)| *entry_irq == irq && *entry_cpu == cpu)
        {
            *state = enabled;
        } else {
            states.push((irq, cpu, enabled));
        }
    }

    fn calls(&self) -> Vec<OpCall> {
        self.inner.calls.lock().unwrap().clone()
    }

    fn clear_calls(&self) {
        self.inner.calls.lock().unwrap().clear();
    }

    fn set_line_state_from_calls(&self, irq: usize, cpu: Option<usize>, enabled: bool) {
        let mut states = self.inner.line_enabled.lock().unwrap();
        if let Some((_, _, state)) = states
            .iter_mut()
            .find(|(entry_irq, entry_cpu, _)| *entry_irq == irq && *entry_cpu == cpu)
        {
            *state = enabled;
        } else {
            states.push((irq, cpu, enabled));
        }
    }
}

// SAFETY: The mock invokes the CPU thunk inline before returning and never
// retains its raw argument. Shared state is synchronized or atomic.
unsafe impl IrqOps for MockOps {
    type LocalIrqState = ();

    fn current_cpu(&self) -> CpuId {
        self.inner
            .current_cpu_snapshots
            .fetch_add(1, Ordering::SeqCst);
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

    fn local_irq_save(&self) -> Self::LocalIrqState {
        self.inner.local_irq_depth.fetch_add(1, Ordering::SeqCst);
    }

    fn local_irq_restore(&self, _state: Self::LocalIrqState) {
        let previous = self.inner.local_irq_depth.fetch_sub(1, Ordering::SeqCst);
        assert!(previous > 0, "mock local IRQ nesting underflowed");
    }

    fn run_on_cpu_sync(
        &self,
        cpu: CpuId,
        f: unsafe fn(*mut ()),
        arg: *mut (),
    ) -> Result<(), IrqError> {
        let old = self.inner.current_cpu.load(Ordering::SeqCst);
        if old != cpu.0 && self.inner.in_irq.load(Ordering::SeqCst) {
            return Err(IrqError::InIrqContext);
        }
        self.inner.cpu_sync_calls.fetch_add(1, Ordering::SeqCst);
        self.set_current_cpu(cpu.0);
        unsafe { f(arg) };
        self.set_current_cpu(old);
        Ok(())
    }

    fn set_enabled(&self, irq: IrqId, cpu: Option<CpuId>, enabled: bool) -> Result<(), IrqError> {
        let raw_irq = raw_irq(irq);
        self.inner.calls.lock().unwrap().push(OpCall::SetEnabled {
            irq: raw_irq,
            cpu: cpu.map(|cpu| cpu.0),
            enabled,
        });
        if self.inner.fail_set_enabled.lock().unwrap().contains(&(
            raw_irq,
            cpu.map(|cpu| cpu.0),
            enabled,
        )) {
            return Err(IrqError::Controller);
        }
        self.set_line_state_from_calls(raw_irq, cpu.map(|cpu| cpu.0), enabled);
        Ok(())
    }

    fn set_affinity(&self, irq: IrqId, affinity: IrqAffinity) -> Result<(), IrqError> {
        let raw_irq = raw_irq(irq);
        self.inner.calls.lock().unwrap().push(OpCall::SetAffinity {
            irq: raw_irq,
            affinity,
        });
        let blocker = self.inner.affinity_blocker.lock().unwrap().take();
        if let Some(blocker) = blocker {
            blocker.entered.wait();
            blocker.release.wait();
        }
        if self.inner.fail_set_affinity.load(Ordering::SeqCst) {
            return Err(IrqError::Controller);
        }
        Ok(())
    }

    fn is_enabled(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<bool, IrqError> {
        let raw_irq = raw_irq(irq);
        self.inner.calls.lock().unwrap().push(OpCall::IsEnabled {
            irq: raw_irq,
            cpu: cpu.map(|cpu| cpu.0),
        });
        if self.inner.unsupported_status.load(Ordering::SeqCst) {
            return Err(IrqError::Unsupported);
        }
        Ok(self
            .inner
            .line_enabled
            .lock()
            .unwrap()
            .iter()
            .find(|(entry_irq, entry_cpu, _)| {
                *entry_irq == raw_irq && *entry_cpu == cpu.map(|cpu| cpu.0)
            })
            .map(|(_, _, enabled)| *enabled)
            .unwrap_or(true))
    }

    fn is_pending(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<bool, IrqError> {
        let raw_irq = raw_irq(irq);
        self.inner.calls.lock().unwrap().push(OpCall::IsPending {
            irq: raw_irq,
            cpu: cpu.map(|cpu| cpu.0),
        });
        if self.inner.unsupported_status.load(Ordering::SeqCst) {
            return Err(IrqError::Unsupported);
        }
        Ok(false)
    }

    fn is_in_service(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<bool, IrqError> {
        let raw_irq = raw_irq(irq);
        self.inner.calls.lock().unwrap().push(OpCall::IsInService {
            irq: raw_irq,
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

#[test]
fn request_restores_enabled_line_without_hal_enable() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);

    let handle = registry.request(irq(30), count_request(&counter)).unwrap();

    assert_eq!(
        ops.calls(),
        vec![
            OpCall::IsEnabled { irq: 30, cpu: None },
            OpCall::SetEnabled {
                irq: 30,
                cpu: None,
                enabled: false,
            },
            OpCall::SetEnabled {
                irq: 30,
                cpu: None,
                enabled: true,
            },
        ]
    );
    ops.set_unsupported_status(true);
    let status = registry.status(handle).unwrap();
    assert!(status.action_enabled);
    assert!(status.line_enabled);
    assert_eq!(registry.dispatch(irq(30), CpuId(0)).called, 1);
}

#[test]
fn explicitly_enabled_request_enables_a_disabled_line() {
    let ops = MockOps::with_cpus(1);
    ops.set_line_enabled(31, None, false);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);

    let handle = registry.request(irq(31), count_request(&counter)).unwrap();

    assert!(ops.calls().contains(&OpCall::SetEnabled {
        irq: 31,
        cpu: None,
        enabled: true,
    }));
    ops.set_unsupported_status(true);
    let status = registry.status(handle).unwrap();
    assert!(status.action_enabled);
    assert!(status.line_enabled);
    assert_eq!(registry.dispatch(irq(31), CpuId(0)).called, 1);
}

#[test]
fn disabled_request_keeps_an_unshared_backing_line_masked() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);

    let handle = registry
        .request(irq(32), count_request(&counter).auto_enable(AutoEnable::No))
        .unwrap();

    assert_eq!(
        ops.calls(),
        vec![
            OpCall::IsEnabled { irq: 32, cpu: None },
            OpCall::SetEnabled {
                irq: 32,
                cpu: None,
                enabled: false,
            },
        ]
    );
    ops.set_unsupported_status(true);
    let status = registry.status(handle).unwrap();
    assert!(!status.action_enabled);
    assert!(!status.line_enabled);
    assert_eq!(registry.dispatch(irq(32), CpuId(0)).called, 0);
}

#[test]
fn irq_request_exposes_auto_enable_mode() {
    assert_eq!(
        IrqRequest::new(|_| IrqReturn::Handled).auto_enable_mode(),
        AutoEnable::No
    );
    assert_eq!(
        IrqRequest::new_concurrent(|_| IrqReturn::Handled).auto_enable_mode(),
        AutoEnable::No
    );
    assert_eq!(
        IrqRequest::new(|_| IrqReturn::Handled)
            .auto_enable(AutoEnable::Yes)
            .auto_enable_mode(),
        AutoEnable::Yes
    );
}

#[test]
fn request_requires_explicit_enable_before_dispatch() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops);
    let counter = Arc::new(AtomicUsize::new(0));
    let handler_counter = Arc::clone(&counter);

    let handle = registry
        .request(
            irq(65),
            IrqRequest::new(move |_| {
                handler_counter.fetch_add(1, Ordering::SeqCst);
                IrqReturn::Handled
            }),
        )
        .unwrap();

    assert_eq!(registry.dispatch(irq(65), CpuId(0)).called, 0);
    let status = registry.status(handle).unwrap();
    assert!(!status.action_enabled);
    assert!(!status.line_enabled);

    registry.enable(handle).unwrap();
    assert_eq!(registry.dispatch(irq(65), CpuId(0)).called, 1);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn request_from_irq_context_is_rejected_before_controller_transaction() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);
    let request = count_request(&counter);

    ops.clear_calls();
    ops.set_in_irq(true);
    assert_eq!(
        registry.request(irq(68), request),
        Err(IrqError::InIrqContext)
    );
    ops.set_in_irq(false);

    assert!(ops.calls().is_empty());
    assert_eq!(registry.dispatch(irq(68), CpuId(0)).called, 0);
}

#[test]
fn boxed_callback_persists_captured_state() {
    let registry = Registry::new(MockOps::with_cpus(1));
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = calls.clone();

    registry
        .request(
            irq(46),
            enabled_request(move |ctx| {
                assert_eq!(ctx.irq, irq(46));
                callback_calls.fetch_add(1, Ordering::SeqCst);
                IrqReturn::Wake
            }),
        )
        .unwrap();

    let first = registry.dispatch(irq(46), CpuId(0));
    let second = registry.dispatch(irq(46), CpuId(0));

    assert!(first.handled);
    assert!(first.wake);
    assert_eq!(first.called, 1);
    assert!(second.handled);
    assert!(second.wake);
    assert_eq!(second.called, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn quench_and_wake_masks_shared_line_until_recovery_releases_owner() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops);
    let peer_calls = Arc::new(AtomicUsize::new(0));
    let quench_calls = Arc::new(AtomicUsize::new(0));

    let peer_counter = Arc::clone(&peer_calls);
    let peer = registry
        .request(
            irq(51),
            enabled_request(move |_| {
                peer_counter.fetch_add(1, Ordering::SeqCst);
                IrqReturn::Handled
            })
            .share_mode(ShareMode::Shared),
        )
        .unwrap();
    let quench_counter = Arc::clone(&quench_calls);
    let failing = registry
        .request(
            irq(51),
            enabled_request(move |_| {
                quench_counter.fetch_add(1, Ordering::SeqCst);
                IrqReturn::QuenchAndWake
            })
            .share_mode(ShareMode::Shared),
        )
        .unwrap();

    let first = registry.dispatch(irq(51), CpuId(0));
    assert!(first.handled);
    assert!(first.wake);
    assert_eq!(first.called, 2);
    assert!(!registry.status(failing).unwrap().action_enabled);
    assert!(registry.status(failing).unwrap().quench_owned);
    assert!(!registry.status(failing).unwrap().line_enabled);
    assert!(registry.status(peer).unwrap().action_enabled);

    registry.release_quench(failing).unwrap();
    assert!(!registry.status(failing).unwrap().quench_owned);
    assert!(registry.status(peer).unwrap().line_enabled);

    let second = registry.dispatch(irq(51), CpuId(0));
    assert!(second.handled);
    assert!(!second.wake);
    assert_eq!(second.called, 1);
    assert_eq!(quench_calls.load(Ordering::SeqCst), 1);
    assert_eq!(peer_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn quench_and_wake_masks_an_unshared_line_before_dispatch_returns() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let action = registry
        .request(irq(52), enabled_request(|_| IrqReturn::QuenchAndWake))
        .unwrap();
    ops.clear_calls();

    let outcome = registry.dispatch(irq(52), CpuId(0));
    assert!(outcome.handled);
    assert!(outcome.wake);
    let status = registry.status(action).unwrap();
    assert!(!status.action_enabled);
    assert!(!status.line_enabled);
    assert!(ops.calls().contains(&OpCall::SetEnabled {
        irq: 52,
        cpu: None,
        enabled: false,
    }));
    assert_eq!(registry.dispatch(irq(52), CpuId(0)).called, 0);
}

#[test]
#[should_panic(expected = "IRQ controller failed the emergency line quench invariant")]
fn emergency_quench_mask_failure_is_fatal_before_dispatch_returns() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    registry
        .request(irq(56), enabled_request(|_| IrqReturn::QuenchAndWake))
        .unwrap();
    ops.fail_set_enabled(56, None, false);

    let _ = registry.dispatch(irq(56), CpuId(0));
}

#[test]
fn every_shared_quench_owner_must_release_before_the_line_reopens() {
    let registry = Registry::new(MockOps::with_cpus(1));
    let peer = registry
        .request(
            irq(53),
            enabled_request(|_| IrqReturn::Handled).share_mode(ShareMode::Shared),
        )
        .unwrap();
    let first = registry
        .request(
            irq(53),
            enabled_request(|_| IrqReturn::QuenchAndWake).share_mode(ShareMode::Shared),
        )
        .unwrap();
    let second = registry
        .request(
            irq(53),
            enabled_request(|_| IrqReturn::QuenchAndWake).share_mode(ShareMode::Shared),
        )
        .unwrap();

    registry.dispatch(irq(53), CpuId(0));
    assert!(!registry.status(peer).unwrap().line_enabled);

    registry.release_quench(first).unwrap();
    assert!(!registry.status(peer).unwrap().line_enabled);

    registry.release_quench(second).unwrap();
    assert!(registry.status(peer).unwrap().line_enabled);
}

#[test]
fn per_cpu_quench_masks_only_the_cpu_that_observed_the_failure() {
    let ops = MockOps::with_cpus(2);
    let registry = Registry::new(ops.clone());
    let cpus = CpuMask::first_n(2);
    let peer = registry
        .request(
            irq(55),
            enabled_concurrent_request(|_| IrqReturn::Handled)
                .scope(IrqScope::PerCpu { cpus })
                .share_mode(ShareMode::Shared),
        )
        .unwrap();
    let failing = registry
        .request(
            irq(55),
            enabled_concurrent_request(|_| IrqReturn::QuenchAndWake)
                .scope(IrqScope::PerCpu { cpus })
                .share_mode(ShareMode::Shared),
        )
        .unwrap();

    registry.dispatch(irq(55), CpuId(0));
    ops.set_current_cpu(0);
    assert!(!registry.status(peer).unwrap().line_enabled);
    ops.set_current_cpu(1);
    assert!(registry.status(peer).unwrap().line_enabled);

    registry.release_per_cpu_quench(failing, CpuId(0)).unwrap();
    ops.set_current_cpu(0);
    assert!(registry.status(peer).unwrap().line_enabled);
}

#[test]
fn per_cpu_quench_never_rendezvous_with_a_remote_cpu_from_hard_irq() {
    let ops = MockOps::with_cpus(2);
    let registry = Registry::new(ops.clone());
    let action = registry
        .request(
            irq(58),
            enabled_concurrent_request(|ctx| {
                if ctx.cpu == CpuId(0) {
                    IrqReturn::QuenchAndWake
                } else {
                    IrqReturn::Handled
                }
            })
            .scope(IrqScope::PerCpu {
                cpus: CpuMask::first_n(2),
            }),
        )
        .unwrap();

    ops.set_current_cpu(0);
    ops.set_in_irq(true);
    let outcome = registry.dispatch(irq(58), CpuId(0));
    ops.set_in_irq(false);
    assert!(outcome.handled);
    assert!(outcome.wake);
    assert!(registry.status(action).unwrap().quench_owned);
    assert!(!registry.status(action).unwrap().line_enabled);

    ops.set_current_cpu(1);
    assert!(registry.status(action).unwrap().line_enabled);
    ops.set_in_irq(true);
    assert_eq!(registry.dispatch(irq(58), CpuId(1)).called, 1);
    ops.set_in_irq(false);

    registry.release_per_cpu_quench(action, CpuId(0)).unwrap();
    registry.free(action).unwrap();
}

#[test]
fn releasing_one_per_cpu_quench_does_not_release_another_cpu() {
    let ops = MockOps::with_cpus(2);
    let registry = Registry::new(ops.clone());
    let action = registry
        .request(
            irq(62),
            enabled_concurrent_request(|_| IrqReturn::QuenchAndWake).scope(IrqScope::PerCpu {
                cpus: CpuMask::first_n(2),
            }),
        )
        .unwrap();

    registry.dispatch(irq(62), CpuId(0));
    registry.dispatch(irq(62), CpuId(1));
    ops.set_current_cpu(0);
    assert!(!registry.status(action).unwrap().line_enabled);
    ops.set_current_cpu(1);
    assert!(!registry.status(action).unwrap().line_enabled);

    ops.set_current_cpu(0);
    registry.release_per_cpu_quench(action, CpuId(0)).unwrap();
    assert!(registry.status(action).unwrap().line_enabled);
    ops.set_current_cpu(1);
    assert!(
        !registry.status(action).unwrap().line_enabled,
        "CPU1 must retain independent quench ownership"
    );

    registry.release_per_cpu_quench(action, CpuId(1)).unwrap();
    registry.free(action).unwrap();
}

#[test]
fn quench_release_requires_an_explicit_matching_scope() {
    let registry = Registry::new(MockOps::with_cpus(2));
    let global = registry
        .request(irq(63), enabled_request(|_| IrqReturn::QuenchAndWake))
        .unwrap();
    registry.dispatch(irq(63), CpuId(0));
    assert_eq!(
        registry.release_per_cpu_quench(global, CpuId(0)),
        Err(IrqError::InvalidCpu)
    );
    assert!(registry.status(global).unwrap().quench_owned);
    registry.release_quench(global).unwrap();
    registry.free(global).unwrap();

    let per_cpu = registry
        .request(
            irq(64),
            enabled_concurrent_request(|_| IrqReturn::QuenchAndWake).scope(IrqScope::PerCpu {
                cpus: CpuMask::first_n(2),
            }),
        )
        .unwrap();
    registry.dispatch(irq(64), CpuId(0));
    assert_eq!(registry.release_quench(per_cpu), Err(IrqError::InvalidCpu));
    assert_eq!(
        registry.release_per_cpu_quench(per_cpu, CpuId(2)),
        Err(IrqError::InvalidCpu)
    );
    assert!(registry.status(per_cpu).unwrap().quench_owned);
    registry.release_per_cpu_quench(per_cpu, CpuId(0)).unwrap();
    registry.free(per_cpu).unwrap();
}

#[test]
fn quenched_action_cannot_be_enabled_or_freed_before_release() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let action = registry
        .request(irq(54), enabled_request(|_| IrqReturn::QuenchAndWake))
        .unwrap();

    registry.dispatch(irq(54), CpuId(0));
    assert_eq!(registry.enable(action), Err(IrqError::Busy));
    assert_eq!(registry.free(action), Err(IrqError::Busy));
    ops.set_in_irq(true);
    assert_eq!(registry.release_quench(action), Err(IrqError::InIrqContext));
    ops.set_in_irq(false);

    registry.release_quench(action).unwrap();
    registry.free(action).unwrap();
}

#[test]
fn quenched_action_cannot_start_an_async_drain_generation() {
    unsafe fn notify_drain(data: usize) {
        let notifications = unsafe { &*(data as *const AtomicUsize) };
        notifications.fetch_add(1, Ordering::SeqCst);
    }

    let registry = Registry::new(MockOps::with_cpus(1));
    let action = registry
        .request(irq(59), enabled_request(|_| IrqReturn::QuenchAndWake))
        .unwrap();
    registry.dispatch(irq(59), CpuId(0));

    let notifications = Box::leak(Box::new(AtomicUsize::new(0)));
    let wake = Box::leak(Box::new(unsafe {
        // SAFETY: both allocations are leaked for shutdown lifetime, and the
        // callback performs only a lock-free atomic increment.
        IrqDrainWake::new(notifications as *const AtomicUsize as usize, notify_drain)
    }));
    assert_eq!(registry.disable_async(action, wake), Err(IrqError::Busy));
    assert_eq!(notifications.load(Ordering::SeqCst), 0);

    registry.release_quench(action).unwrap();
    registry.free(action).unwrap();
}

#[test]
fn boxed_callback_rejects_concurrent_execution() {
    let registry = Registry::new(MockOps::with_cpus(1));

    let err = registry
        .request(
            irq(47),
            enabled_request(|_| IrqReturn::Handled).execution(IrqExecution::Concurrent),
        )
        .unwrap_err();

    assert_eq!(err, IrqError::Busy);
}

#[test]
fn boxed_callback_is_non_reentrant() {
    let registry = Arc::new(Registry::new(MockOps::with_cpus(1)));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_entered = entered.clone();
    let callback_release = release.clone();
    let callback_calls = calls.clone();

    registry
        .request(
            irq(48),
            enabled_request(move |_| {
                callback_calls.fetch_add(1, Ordering::SeqCst);
                callback_entered.wait();
                callback_release.wait();
                IrqReturn::Handled
            }),
        )
        .unwrap();

    let dispatch_registry = registry.clone();
    let dispatch_thread = thread::spawn(move || dispatch_registry.dispatch(irq(48), CpuId(0)));
    entered.wait();

    let nested = registry.dispatch(irq(48), CpuId(0));
    assert!(!nested.handled);
    assert_eq!(nested.called, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    release.wait();
    let outcome = dispatch_thread.join().unwrap();
    assert!(outcome.handled);
    assert_eq!(outcome.called, 1);
}

#[test]
fn shared_request_temporarily_disables_existing_line_and_restores_it() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let first = AtomicUsize::new(0);
    let second = AtomicUsize::new(0);

    registry
        .request(irq(33), count_request(&first).share_mode(ShareMode::Shared))
        .unwrap();
    ops.clear_calls();

    registry
        .request(
            irq(33),
            count_request(&second).share_mode(ShareMode::Shared),
        )
        .unwrap();

    assert_eq!(
        ops.calls(),
        vec![
            OpCall::IsEnabled { irq: 33, cpu: None },
            OpCall::SetEnabled {
                irq: 33,
                cpu: None,
                enabled: false,
            },
            OpCall::SetEnabled {
                irq: 33,
                cpu: None,
                enabled: true,
            },
        ]
    );
    let outcome = registry.dispatch(irq(33), CpuId(0));
    assert!(outcome.handled);
    assert_eq!(outcome.called, 2);
}

#[test]
fn failed_request_restores_line_and_drops_new_action() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let first = AtomicUsize::new(0);
    let rejected = AtomicUsize::new(0);

    registry.request(irq(34), count_request(&first)).unwrap();
    ops.clear_calls();

    let err = registry
        .request(
            irq(34),
            count_request(&rejected).share_mode(ShareMode::Shared),
        )
        .unwrap_err();

    assert_eq!(err, IrqError::Busy);
    assert_eq!(
        ops.calls(),
        vec![
            OpCall::IsEnabled { irq: 34, cpu: None },
            OpCall::SetEnabled {
                irq: 34,
                cpu: None,
                enabled: false,
            },
            OpCall::SetEnabled {
                irq: 34,
                cpu: None,
                enabled: true,
            },
        ]
    );
    assert_eq!(registry.dispatch(irq(34), CpuId(0)).called, 1);
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(rejected.load(Ordering::SeqCst), 0);
}

#[test]
fn failed_request_never_publishes_handler_while_affinity_is_uncommitted() {
    let ops = MockOps::with_cpus(1);
    ops.set_fail_set_affinity(true);
    let affinity = ops.block_next_affinity();
    let registry = Arc::new(Registry::new(ops.clone()));
    let request_registry = Arc::clone(&registry);
    let request = thread::spawn(move || {
        request_registry.request(
            irq(61),
            enabled_request(|_| IrqReturn::QuenchAndWake).affinity(IrqAffinity::Fixed(CpuId(0))),
        )
    });

    affinity.entered.wait();
    let outcome = registry.dispatch(irq(61), CpuId(0));
    assert_eq!(outcome.called, 0);
    assert!(!outcome.handled);
    assert!(!outcome.wake);
    affinity.release.wait();
    assert_eq!(request.join().unwrap(), Err(IrqError::Controller));

    ops.set_fail_set_affinity(false);
    let replacement = registry
        .request(irq(61), enabled_request(|_| IrqReturn::Handled))
        .expect("a failed registration must not retain a hidden quench owner");
    registry.free(replacement).unwrap();
}

#[test]
fn failed_restore_after_request_drops_new_action() {
    let ops = MockOps::with_cpus(1);
    ops.fail_set_enabled(36, None, true);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);

    let err = registry
        .request(irq(36), count_request(&counter))
        .unwrap_err();

    assert_eq!(err, IrqError::Controller);
    assert_eq!(
        ops.calls(),
        vec![
            OpCall::IsEnabled { irq: 36, cpu: None },
            OpCall::SetEnabled {
                irq: 36,
                cpu: None,
                enabled: false,
            },
            OpCall::SetEnabled {
                irq: 36,
                cpu: None,
                enabled: true,
            },
            OpCall::SetEnabled {
                irq: 36,
                cpu: None,
                enabled: true,
            },
        ]
    );
    assert_eq!(registry.dispatch(irq(36), CpuId(0)).called, 0);
}

#[test]
fn failed_percpu_snapshot_restores_already_disabled_cpu_lines() {
    let ops = MockOps::with_cpus(2);
    ops.fail_set_enabled(37, Some(1), false);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);
    let mut cpus = CpuMask::empty();
    cpus.insert(CpuId(0));
    cpus.insert(CpuId(1));

    let err = registry
        .request(
            irq(37),
            count_request(&counter).scope(IrqScope::PerCpu { cpus }),
        )
        .unwrap_err();

    assert_eq!(err, IrqError::Controller);
    assert_eq!(
        ops.calls(),
        vec![
            OpCall::IsEnabled {
                irq: 37,
                cpu: Some(0),
            },
            OpCall::SetEnabled {
                irq: 37,
                cpu: Some(0),
                enabled: false,
            },
            OpCall::IsEnabled {
                irq: 37,
                cpu: Some(1),
            },
            OpCall::SetEnabled {
                irq: 37,
                cpu: Some(1),
                enabled: false,
            },
            OpCall::SetEnabled {
                irq: 37,
                cpu: Some(0),
                enabled: true,
            },
        ]
    );
    assert_eq!(registry.dispatch(irq(37), CpuId(0)).called, 0);
    assert_eq!(registry.dispatch(irq(37), CpuId(1)).called, 0);
}

#[test]
fn disabled_percpu_request_keeps_target_cpu_line_masked() {
    let ops = MockOps::with_cpus(4);
    ops.set_current_cpu(0);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);

    registry
        .request(
            irq(35),
            count_request(&counter)
                .scope(IrqScope::PerCpu {
                    cpus: CpuMask::from_cpu(CpuId(2)),
                })
                .auto_enable(AutoEnable::No),
        )
        .unwrap();

    assert_eq!(
        ops.calls(),
        vec![
            OpCall::IsEnabled {
                irq: 35,
                cpu: Some(2),
            },
            OpCall::SetEnabled {
                irq: 35,
                cpu: Some(2),
                enabled: false,
            },
        ]
    );
    assert_eq!(ops.inner.cpu_sync_calls.load(Ordering::SeqCst), 1);
    assert_eq!(registry.dispatch(irq(35), CpuId(2)).called, 0);
}

#[test]
fn same_hwirq_in_different_domains_are_independent_descriptors() {
    let registry = Registry::new(MockOps::with_cpus(1));
    let first = AtomicUsize::new(0);
    let second = AtomicUsize::new(0);
    let irq_a = domain_irq(TEST_DOMAIN_A, 5);
    let irq_b = domain_irq(TEST_DOMAIN_B, 5);

    registry.request(irq_a, count_request(&first)).unwrap();
    registry.request(irq_b, count_request(&second)).unwrap();

    assert_eq!(registry.dispatch(irq_a, CpuId(0)).called, 1);
    assert_eq!(registry.dispatch(irq_b, CpuId(0)).called, 1);
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(second.load(Ordering::SeqCst), 1);
}

#[test]
fn dynamic_shared_actions_all_dispatch() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops);
    let mut counters = Vec::new();

    for _ in 0..64 {
        counters.push(Box::new(AtomicUsize::new(0)));
        let counter = counters.last().unwrap();
        registry
            .request(irq(7), count_request(counter).share_mode(ShareMode::Shared))
            .unwrap();
    }

    let outcome = registry.dispatch(irq(7), CpuId(0));
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
fn shared_actions_may_use_independent_execution_contracts() {
    let registry = Registry::new(MockOps::with_cpus(1));
    let serialized = AtomicUsize::new(0);
    let concurrent = AtomicUsize::new(0);
    let concurrent_ptr = &concurrent as *const AtomicUsize as usize;

    registry
        .request(
            irq(44),
            count_request(&serialized).share_mode(ShareMode::Shared),
        )
        .unwrap();
    registry
        .request(
            irq(44),
            enabled_concurrent_request(move |_| {
                let counter = unsafe { &*(concurrent_ptr as *const AtomicUsize) };
                counter.fetch_add(1, Ordering::SeqCst);
                IrqReturn::Handled
            })
            .share_mode(ShareMode::Shared),
        )
        .unwrap();

    let outcome = registry.dispatch(irq(44), CpuId(0));
    assert_eq!(outcome.called, 2);
    assert_eq!(serialized.load(Ordering::SeqCst), 1);
    assert_eq!(concurrent.load(Ordering::SeqCst), 1);
}

#[test]
fn shared_dispatch_does_not_short_circuit_on_handled() {
    let registry = Registry::new(MockOps::with_cpus(1));
    let handled_counter = AtomicUsize::new(0);
    let wake_counter = AtomicUsize::new(0);

    registry
        .request(
            irq(22),
            count_request(&handled_counter).share_mode(ShareMode::Shared),
        )
        .unwrap();
    registry
        .request(
            irq(22),
            wake_request(&wake_counter).share_mode(ShareMode::Shared),
        )
        .unwrap();

    let outcome = registry.dispatch(irq(22), CpuId(0));
    assert!(outcome.handled);
    assert!(outcome.wake);
    assert_eq!(outcome.called, 2);
    assert_eq!(handled_counter.load(Ordering::SeqCst), 1);
    assert_eq!(wake_counter.load(Ordering::SeqCst), 1);
}

#[test]
fn shared_handlers_may_all_decline_the_interrupt() {
    let registry = Registry::new(MockOps::with_cpus(1));

    registry
        .request(
            irq(66),
            enabled_request(|_| IrqReturn::Unhandled).share_mode(ShareMode::Shared),
        )
        .unwrap();
    registry
        .request(
            irq(66),
            enabled_request(|_| IrqReturn::Unhandled).share_mode(ShareMode::Shared),
        )
        .unwrap();

    let outcome = registry.dispatch(irq(66), CpuId(0));
    assert!(!outcome.handled);
    assert!(!outcome.wake);
    assert_eq!(outcome.called, 2);
}

#[test]
fn disabled_shared_joiner_does_not_mask_an_enabled_peer() {
    let registry = Registry::new(MockOps::with_cpus(1));
    let peer_calls = AtomicUsize::new(0);
    let disabled_calls = AtomicUsize::new(0);

    registry
        .request(
            irq(67),
            count_request(&peer_calls).share_mode(ShareMode::Shared),
        )
        .unwrap();
    let disabled = registry
        .request(
            irq(67),
            count_request(&disabled_calls)
                .share_mode(ShareMode::Shared)
                .auto_enable(AutoEnable::No),
        )
        .unwrap();

    let status = registry.status(disabled).unwrap();
    assert!(!status.action_enabled);
    assert!(status.line_enabled);
    let outcome = registry.dispatch(irq(67), CpuId(0));
    assert!(outcome.handled);
    assert_eq!(outcome.called, 1);
    assert_eq!(peer_calls.load(Ordering::SeqCst), 1);
    assert_eq!(disabled_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn disabled_or_freed_shared_action_is_skipped_but_peers_run() {
    let registry = Registry::new(MockOps::with_cpus(1));
    let disabled_or_freed = AtomicUsize::new(0);
    let peer = AtomicUsize::new(0);

    let disabled_or_freed_handle = registry
        .request(
            irq(23),
            count_request(&disabled_or_freed).share_mode(ShareMode::Shared),
        )
        .unwrap();
    registry
        .request(irq(23), count_request(&peer).share_mode(ShareMode::Shared))
        .unwrap();

    registry.disable(disabled_or_freed_handle).unwrap();
    let outcome = registry.dispatch(irq(23), CpuId(0));
    assert!(outcome.handled);
    assert!(!outcome.wake);
    assert_eq!(outcome.called, 1);
    assert_eq!(disabled_or_freed.load(Ordering::SeqCst), 0);
    assert_eq!(peer.load(Ordering::SeqCst), 1);

    registry.free(disabled_or_freed_handle).unwrap();
    let outcome = registry.dispatch(irq(23), CpuId(0));
    assert!(outcome.handled);
    assert!(!outcome.wake);
    assert_eq!(outcome.called, 1);
    assert_eq!(disabled_or_freed.load(Ordering::SeqCst), 0);
    assert_eq!(peer.load(Ordering::SeqCst), 2);
}

#[test]
fn exclusive_and_shared_conflict() {
    let registry = Registry::new(MockOps::with_cpus(1));
    let counter = AtomicUsize::new(0);

    registry
        .request(irq(3), count_request(&counter).auto_enable(AutoEnable::No))
        .unwrap();

    let err = registry
        .request(
            irq(3),
            count_request(&counter)
                .share_mode(ShareMode::Shared)
                .auto_enable(AutoEnable::No),
        )
        .unwrap_err();

    assert_eq!(err, IrqError::Busy);
}

#[test]
fn fixed_affinity_is_set_before_restoring_enabled_line() {
    let ops = MockOps::with_cpus(2);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);

    registry
        .request(
            irq(41),
            count_request(&counter).affinity(IrqAffinity::Fixed(CpuId(1))),
        )
        .unwrap();

    assert_eq!(
        ops.calls(),
        vec![
            OpCall::IsEnabled { irq: 41, cpu: None },
            OpCall::SetEnabled {
                irq: 41,
                cpu: None,
                enabled: false,
            },
            OpCall::SetAffinity {
                irq: 41,
                affinity: IrqAffinity::Fixed(CpuId(1)),
            },
            OpCall::SetEnabled {
                irq: 41,
                cpu: None,
                enabled: true,
            },
        ]
    );
}

#[test]
fn fixed_affinity_rejects_offline_cpu_and_controller_failure() {
    let ops = MockOps::with_cpus(2);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);

    ops.set_online(1, false);
    assert_eq!(
        registry.request(
            irq(42),
            count_request(&counter).affinity(IrqAffinity::Fixed(CpuId(1))),
        ),
        Err(IrqError::CpuOffline)
    );

    ops.set_online(1, true);
    ops.fail_set_affinity();
    assert_eq!(
        registry.request(
            irq(42),
            count_request(&counter).affinity(IrqAffinity::Fixed(CpuId(1))),
        ),
        Err(IrqError::Controller)
    );
}

#[test]
fn shared_actions_must_use_same_affinity() {
    let registry = Registry::new(MockOps::with_cpus(2));
    let first = AtomicUsize::new(0);
    let second = AtomicUsize::new(0);

    registry
        .request(
            irq(43),
            count_request(&first)
                .share_mode(ShareMode::Shared)
                .affinity(IrqAffinity::Fixed(CpuId(0)))
                .execution(IrqExecution::NonReentrant),
        )
        .unwrap();

    assert_eq!(
        registry.request(
            irq(43),
            count_request(&second)
                .share_mode(ShareMode::Shared)
                .affinity(IrqAffinity::Fixed(CpuId(1)))
                .execution(IrqExecution::NonReentrant),
        ),
        Err(IrqError::Busy)
    );
}

#[test]
fn shared_any_action_inherits_the_existing_fixed_line_affinity() {
    let registry = Registry::new(MockOps::with_cpus(2));
    let fixed = AtomicUsize::new(0);
    let unconstrained = AtomicUsize::new(0);

    registry
        .request(
            irq(45),
            count_request(&fixed)
                .share_mode(ShareMode::Shared)
                .affinity(IrqAffinity::Fixed(CpuId(0))),
        )
        .unwrap();
    registry
        .request(
            irq(45),
            count_request(&unconstrained).share_mode(ShareMode::Shared),
        )
        .unwrap();

    let outcome = registry.dispatch(irq(45), CpuId(0));
    assert_eq!(outcome.called, 2);
    assert_eq!(fixed.load(Ordering::SeqCst), 1);
    assert_eq!(unconstrained.load(Ordering::SeqCst), 1);
}

#[test]
fn free_waits_for_inflight_dispatch_and_detaches_action() {
    struct Blocker {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
        calls: AtomicUsize,
    }

    let registry = Arc::new(Registry::new(MockOps::with_cpus(1)));
    let blocker = Arc::new(Blocker {
        entered: Arc::new(Barrier::new(2)),
        release: Arc::new(Barrier::new(2)),
        calls: AtomicUsize::new(0),
    });
    let handler_blocker = blocker.clone();
    let handle = registry
        .request(
            irq(11),
            enabled_request(move |_| {
                handler_blocker.calls.fetch_add(1, Ordering::SeqCst);
                handler_blocker.entered.wait();
                handler_blocker.release.wait();
                IrqReturn::Handled
            }),
        )
        .unwrap();

    let dispatch_registry = registry.clone();
    let dispatch_thread = thread::spawn(move || dispatch_registry.dispatch(irq(11), CpuId(0)));

    blocker.entered.wait();

    let free_registry = registry.clone();
    let free_thread = thread::spawn(move || free_registry.free(handle));

    thread::sleep(std::time::Duration::from_millis(30));
    assert!(!free_thread.is_finished());

    blocker.release.wait();
    assert!(dispatch_thread.join().unwrap().handled);
    free_thread.join().unwrap().unwrap();

    let outcome = registry.dispatch(irq(11), CpuId(0));
    assert!(!outcome.handled);
    assert_eq!(outcome.called, 0);
    assert_eq!(blocker.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn non_reentrant_action_skips_nested_dispatch() {
    struct Blocker {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
        calls: AtomicUsize,
    }

    let registry = Arc::new(Registry::new(MockOps::with_cpus(1)));
    let blocker = Arc::new(Blocker {
        entered: Arc::new(Barrier::new(2)),
        release: Arc::new(Barrier::new(2)),
        calls: AtomicUsize::new(0),
    });
    let handler_blocker = blocker.clone();
    registry
        .request(
            irq(44),
            enabled_request(move |_| {
                handler_blocker.calls.fetch_add(1, Ordering::SeqCst);
                handler_blocker.entered.wait();
                handler_blocker.release.wait();
                IrqReturn::Handled
            })
            .execution(IrqExecution::NonReentrant),
        )
        .unwrap();

    let dispatch_registry = registry.clone();
    let dispatch_thread = thread::spawn(move || dispatch_registry.dispatch(irq(44), CpuId(0)));
    blocker.entered.wait();

    let nested = registry.dispatch(irq(44), CpuId(0));
    assert!(!nested.handled);
    assert_eq!(nested.called, 0);
    assert_eq!(blocker.calls.load(Ordering::SeqCst), 1);

    blocker.release.wait();
    let outcome = dispatch_thread.join().unwrap();
    assert!(outcome.handled);
    assert_eq!(outcome.called, 1);
}

#[test]
fn synchronize_waits_for_inflight_dispatch() {
    struct Blocker {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    let registry = Arc::new(Registry::new(MockOps::with_cpus(1)));
    let blocker = Arc::new(Blocker {
        entered: Arc::new(Barrier::new(2)),
        release: Arc::new(Barrier::new(2)),
    });
    let handler_blocker = blocker.clone();
    let handle = registry
        .request(
            irq(45),
            enabled_request(move |_| {
                handler_blocker.entered.wait();
                handler_blocker.release.wait();
                IrqReturn::Handled
            }),
        )
        .unwrap();

    let dispatch_registry = registry.clone();
    let dispatch_thread = thread::spawn(move || dispatch_registry.dispatch(irq(45), CpuId(0)));
    blocker.entered.wait();

    let sync_registry = registry.clone();
    let sync_thread = thread::spawn(move || sync_registry.synchronize(handle));
    thread::sleep(std::time::Duration::from_millis(30));
    assert!(!sync_thread.is_finished());

    blocker.release.wait();
    dispatch_thread.join().unwrap();
    sync_thread.join().unwrap().unwrap();
}

#[test]
fn async_disable_drains_only_the_selected_shared_action() {
    struct SharedBlockers {
        action_entered: Barrier,
        action_release: Barrier,
        peer_entered: Barrier,
        peer_release: Barrier,
    }

    unsafe fn notify_drain(data: usize) {
        let notified = unsafe { &*(data as *const AtomicUsize) };
        notified.fetch_add(1, Ordering::SeqCst);
    }

    let registry = Arc::new(Registry::new(MockOps::with_cpus(1)));
    let blockers = Arc::new(SharedBlockers {
        action_entered: Barrier::new(2),
        action_release: Barrier::new(2),
        peer_entered: Barrier::new(2),
        peer_release: Barrier::new(2),
    });

    let peer_blockers = Arc::clone(&blockers);
    registry
        .request(
            irq(50),
            enabled_request(move |_| {
                peer_blockers.peer_entered.wait();
                peer_blockers.peer_release.wait();
                IrqReturn::Handled
            })
            .share_mode(ShareMode::Shared),
        )
        .unwrap();

    let action_blockers = Arc::clone(&blockers);
    let action = registry
        .request(
            irq(50),
            enabled_request(move |_| {
                action_blockers.action_entered.wait();
                action_blockers.action_release.wait();
                IrqReturn::Handled
            })
            .share_mode(ShareMode::Shared),
        )
        .unwrap();

    let first_registry = Arc::clone(&registry);
    let first = thread::spawn(move || first_registry.dispatch(irq(50), CpuId(0)));
    blockers.action_entered.wait();

    let notified = Box::leak(Box::new(AtomicUsize::new(0)));
    let wake = Box::leak(Box::new(unsafe {
        // SAFETY: `notified` and the wake object are leaked for shutdown
        // lifetime, and the callback performs one lock-free atomic increment.
        IrqDrainWake::new(notified as *const AtomicUsize as usize, notify_drain)
    }));
    let drain = registry.disable_async(action, wake).unwrap();
    assert!(!registry.action_drain_complete(drain).unwrap());

    let peer_registry = Arc::clone(&registry);
    let peer = thread::spawn(move || peer_registry.dispatch(irq(50), CpuId(0)));
    blockers.peer_entered.wait();

    blockers.action_release.wait();
    assert!(first.join().unwrap().handled);
    assert_eq!(notified.load(Ordering::SeqCst), 1);
    assert!(registry.action_drain_complete(drain).unwrap());
    assert!(
        !peer.is_finished(),
        "an unrelated shared action must not be part of this drain token"
    );

    blockers.peer_release.wait();
    assert!(peer.join().unwrap().handled);
}

#[test]
fn immediately_drained_action_notifies_after_releasing_registry_metadata() {
    struct DrainProbe {
        ops: MockOps,
        observed_local_irq_depth: AtomicUsize,
    }

    unsafe fn record_irq_depth(data: usize) {
        let probe = unsafe { &*(data as *const DrainProbe) };
        probe.observed_local_irq_depth.store(
            probe.ops.inner.local_irq_depth.load(Ordering::SeqCst),
            Ordering::SeqCst,
        );
    }

    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let action = registry
        .request(
            irq(60),
            enabled_request(|_| IrqReturn::Handled).auto_enable(AutoEnable::No),
        )
        .unwrap();
    let probe = Box::leak(Box::new(DrainProbe {
        ops,
        observed_local_irq_depth: AtomicUsize::new(usize::MAX),
    }));
    let wake = Box::leak(Box::new(unsafe {
        // SAFETY: the probe and notification are leaked for shutdown lifetime;
        // the callback only reads and writes atomics.
        IrqDrainWake::new(probe as *const DrainProbe as usize, record_irq_depth)
    }));

    let token = registry.disable_async(action, wake).unwrap();
    assert_eq!(probe.observed_local_irq_depth.load(Ordering::SeqCst), 0);
    assert!(registry.action_drain_complete(token).unwrap());
    registry.free(action).unwrap();
}

#[test]
fn detached_fixed_host_allows_any_guest_then_reattaches_disabled() {
    let ops = MockOps::with_cpus(2);
    let registry = Registry::new(ops);
    let host_calls = AtomicUsize::new(0);
    let guest_calls = AtomicUsize::new(0);
    let irq = irq(51);

    let host = registry
        .request(
            irq,
            count_request(&host_calls).affinity(IrqAffinity::Fixed(CpuId(1))),
        )
        .unwrap();
    registry.disable(host).unwrap();
    registry.synchronize(host).unwrap();

    let detached_host = registry.detach_action(host).unwrap();
    assert_eq!(registry.status(host), Err(IrqError::NotFound));

    let guest = registry.request(irq, count_request(&guest_calls)).unwrap();
    assert_eq!(registry.dispatch(irq, CpuId(0)).called, 1);
    assert_eq!(guest_calls.load(Ordering::SeqCst), 1);
    registry.free(guest).unwrap();

    let reattached_host = registry.reattach_action(detached_host).unwrap();
    assert_ne!(reattached_host, host);
    assert_eq!(registry.enable(host), Err(IrqError::NotFound));
    assert!(!registry.status(reattached_host).unwrap().action_enabled);
    assert_eq!(registry.dispatch(irq, CpuId(1)).called, 0);

    registry.enable(reattached_host).unwrap();
    assert_eq!(registry.dispatch(irq, CpuId(1)).called, 1);
    assert_eq!(host_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_reattach_returns_the_unique_action_for_retry() {
    let registry = Registry::new(MockOps::with_cpus(2));
    let host_calls = AtomicUsize::new(0);
    let guest_calls = AtomicUsize::new(0);
    let irq = irq(52);

    let host = registry
        .request(
            irq,
            count_request(&host_calls).affinity(IrqAffinity::Fixed(CpuId(1))),
        )
        .unwrap();
    registry.disable(host).unwrap();
    registry.synchronize(host).unwrap();
    let detached_host = registry.detach_action(host).unwrap();

    let guest = registry.request(irq, count_request(&guest_calls)).unwrap();
    let error = registry.reattach_action(detached_host).unwrap_err();
    assert_eq!(error.reason(), IrqError::Busy);
    let detached_host = error.into_action();

    registry.free(guest).unwrap();
    let host = registry.reattach_action(detached_host).unwrap();
    assert!(!registry.status(host).unwrap().action_enabled);
}

#[test]
fn controller_failure_during_reattach_rolls_back_and_returns_the_action() {
    let ops = MockOps::with_cpus(2);
    let registry = Registry::new(ops.clone());
    let calls = AtomicUsize::new(0);
    let irq = irq(53);

    let host = registry
        .request(
            irq,
            count_request(&calls).affinity(IrqAffinity::Fixed(CpuId(1))),
        )
        .unwrap();
    registry.disable(host).unwrap();
    registry.synchronize(host).unwrap();
    let detached_host = registry.detach_action(host).unwrap();

    ops.set_fail_set_affinity(true);
    let error = registry.reattach_action(detached_host).unwrap_err();
    assert_eq!(error.reason(), IrqError::Controller);
    let detached_host = error.into_action();
    assert_eq!(registry.dispatch(irq, CpuId(1)).called, 0);

    let guest_calls = AtomicUsize::new(0);
    let guest = registry.request(irq, count_request(&guest_calls)).unwrap();
    registry.free(guest).unwrap();

    ops.set_fail_set_affinity(false);
    let host = registry.reattach_action(detached_host).unwrap();
    assert!(!registry.status(host).unwrap().action_enabled);
}

#[test]
fn failed_detach_keeps_the_registered_action_and_handle() {
    let registry = Registry::new(MockOps::with_cpus(1));
    let calls = AtomicUsize::new(0);
    let irq = irq(55);
    let handle = registry.request(irq, count_request(&calls)).unwrap();

    assert!(matches!(
        registry.detach_action(handle),
        Err(IrqError::Busy)
    ));
    assert!(registry.status(handle).unwrap().action_enabled);
    assert_eq!(registry.dispatch(irq, CpuId(0)).called, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    registry.free(handle).unwrap();
}

#[test]
fn in_flight_action_cannot_be_detached_or_destroyed() {
    struct HandlerOwner(Arc<AtomicUsize>);

    impl Drop for HandlerOwner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let registry = Arc::new(Registry::new(MockOps::with_cpus(1)));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let drops = Arc::new(AtomicUsize::new(0));
    let handler_entered = Arc::clone(&entered);
    let handler_release = Arc::clone(&release);
    let handler_owner = HandlerOwner(Arc::clone(&drops));
    let irq = irq(54);
    let handle = registry
        .request(
            irq,
            enabled_request(move |_| {
                let _keep_owner_alive = &handler_owner;
                handler_entered.wait();
                handler_release.wait();
                IrqReturn::Handled
            }),
        )
        .unwrap();

    let dispatch_registry = Arc::clone(&registry);
    let dispatch_thread = thread::spawn(move || dispatch_registry.dispatch(irq, CpuId(0)));
    entered.wait();

    registry.disable(handle).unwrap();
    assert!(matches!(
        registry.detach_action(handle),
        Err(IrqError::Busy)
    ));
    assert_eq!(drops.load(Ordering::SeqCst), 0);

    release.wait();
    assert!(dispatch_thread.join().unwrap().handled);
    registry.synchronize(handle).unwrap();
    let detached = registry.detach_action(handle).unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);

    drop(detached);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn free_racing_quench_retains_the_action_until_recovery_releases_it() {
    let registry = Arc::new(Registry::new(MockOps::with_cpus(1)));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let handler_entered = Arc::clone(&entered);
    let handler_release = Arc::clone(&release);
    let irq = irq(57);
    let handle = registry
        .request(
            irq,
            enabled_request(move |_| {
                handler_entered.wait();
                handler_release.wait();
                IrqReturn::QuenchAndWake
            }),
        )
        .unwrap();

    let dispatch_registry = Arc::clone(&registry);
    let dispatch_thread = thread::spawn(move || dispatch_registry.dispatch(irq, CpuId(0)));
    entered.wait();

    let free_registry = Arc::clone(&registry);
    let free_thread = thread::spawn(move || free_registry.free(handle));
    let transition_deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        match registry.status(handle) {
            Ok(status) if !status.action_enabled => break,
            Err(IrqError::NotFound) => break,
            Ok(_) => {}
            Err(error) => panic!("unexpected action state while free starts: {error:?}"),
        }
        assert!(
            std::time::Instant::now() < transition_deadline,
            "free did not disable the in-flight action"
        );
        thread::yield_now();
    }

    release.wait();
    let outcome = dispatch_thread
        .join()
        .expect("a racing free must not turn a valid quench into a fatal invariant");
    assert!(outcome.handled);
    assert!(outcome.wake);
    assert_eq!(free_thread.join().unwrap(), Err(IrqError::Busy));
    assert!(registry.status(handle).unwrap().quench_owned);

    registry.release_quench(handle).unwrap();
    registry.free(handle).unwrap();
}

#[test]
fn per_cpu_action_dispatches_only_on_matching_cpu() {
    let registry = Registry::new(MockOps::with_cpus(4));
    let counter = AtomicUsize::new(0);
    let cpus = CpuMask::from_cpu(CpuId(2));

    registry
        .request(
            irq(9),
            count_request(&counter).scope(IrqScope::PerCpu { cpus }),
        )
        .unwrap();

    assert_eq!(registry.dispatch(irq(9), CpuId(0)).called, 0);
    assert_eq!(registry.dispatch(irq(9), CpuId(2)).called, 1);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn per_cpu_concurrent_action_allows_parallel_dispatch_on_different_cpus() {
    struct Blocker {
        entered: std::sync::mpsc::Sender<CpuId>,
        release: Barrier,
        calls: AtomicUsize,
    }

    let registry = Arc::new(Registry::new(MockOps::with_cpus(4)));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let blocker = Arc::new(Blocker {
        entered: entered_tx,
        release: Barrier::new(3),
        calls: AtomicUsize::new(0),
    });
    let mut cpus = CpuMask::empty();
    cpus.insert(CpuId(1));
    cpus.insert(CpuId(2));
    let handler_blocker = blocker.clone();

    registry
        .request(
            irq(49),
            enabled_concurrent_request(move |ctx| {
                handler_blocker.calls.fetch_add(1, Ordering::SeqCst);
                handler_blocker.entered.send(ctx.cpu).unwrap();
                handler_blocker.release.wait();
                IrqReturn::Handled
            })
            .scope(IrqScope::PerCpu { cpus }),
        )
        .unwrap();

    let dispatch_registry = registry.clone();
    let first = thread::spawn(move || dispatch_registry.dispatch(irq(49), CpuId(1)));
    assert_eq!(entered_rx.recv().unwrap(), CpuId(1));

    let dispatch_registry = registry.clone();
    let second = thread::spawn(move || dispatch_registry.dispatch(irq(49), CpuId(2)));
    assert_eq!(
        entered_rx.recv_timeout(Duration::from_millis(100)).unwrap(),
        CpuId(2)
    );
    blocker.release.wait();
    let first = first.join().unwrap();
    let second = second.join().unwrap();

    assert_eq!(first.called, 1);
    assert_eq!(second.called, 1);
    assert_eq!(blocker.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn local_per_cpu_enable_uses_run_on_cpu_sync() {
    let ops = MockOps::with_cpus(2);
    ops.set_current_cpu(0);
    ops.set_line_enabled(14, Some(0), false);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);

    let handle = registry
        .request(
            irq(14),
            count_request(&counter)
                .scope(IrqScope::PerCpu {
                    cpus: CpuMask::from_cpu(CpuId(0)),
                })
                .auto_enable(AutoEnable::No),
        )
        .unwrap();
    ops.inner.cpu_sync_calls.store(0, Ordering::SeqCst);
    ops.inner.current_cpu_snapshots.store(0, Ordering::SeqCst);
    ops.clear_calls();

    registry.enable(handle).unwrap();

    assert_eq!(
        ops.inner.cpu_sync_calls.load(Ordering::SeqCst),
        1,
        "CPU-owned line changes must not bypass the pinned execution bridge"
    );
    assert_eq!(
        ops.inner.current_cpu_snapshots.load(Ordering::SeqCst),
        0,
        "CPU-owned line changes must not use the observational CPU snapshot"
    );
    assert!(ops.calls().contains(&OpCall::SetEnabled {
        irq: 14,
        cpu: Some(0),
        enabled: true,
    }));
}

#[test]
fn local_per_cpu_enable_from_irq_context_stays_local() {
    let ops = MockOps::with_cpus(2);
    ops.set_current_cpu(0);
    ops.set_line_enabled(15, Some(0), false);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);

    let handle = registry
        .request(
            irq(15),
            count_request(&counter)
                .scope(IrqScope::PerCpu {
                    cpus: CpuMask::from_cpu(CpuId(0)),
                })
                .auto_enable(AutoEnable::No),
        )
        .unwrap();
    ops.inner.cpu_sync_calls.store(0, Ordering::SeqCst);
    ops.clear_calls();

    ops.set_in_irq(true);
    assert_eq!(registry.enable(handle), Ok(()));
    ops.set_in_irq(false);

    assert_eq!(ops.inner.cpu_sync_calls.load(Ordering::SeqCst), 1);
    assert!(ops.calls().contains(&OpCall::SetEnabled {
        irq: 15,
        cpu: Some(0),
        enabled: true,
    }));
}

#[test]
fn remote_per_cpu_enable_uses_run_on_cpu_sync() {
    let ops = MockOps::with_cpus(4);
    ops.set_current_cpu(0);
    ops.set_line_enabled(12, Some(2), false);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);

    let handle = registry
        .request(
            irq(12),
            count_request(&counter)
                .scope(IrqScope::PerCpu {
                    cpus: CpuMask::from_cpu(CpuId(2)),
                })
                .auto_enable(AutoEnable::No),
        )
        .unwrap();
    ops.inner.cpu_sync_calls.store(0, Ordering::SeqCst);
    ops.clear_calls();

    registry.enable(handle).unwrap();

    assert_eq!(ops.inner.cpu_sync_calls.load(Ordering::SeqCst), 1);
    assert!(ops.calls().contains(&OpCall::SetEnabled {
        irq: 12,
        cpu: Some(2),
        enabled: true,
    }));
}

#[test]
fn remote_per_cpu_enable_from_irq_context_is_rejected_without_ipi() {
    let ops = MockOps::with_cpus(4);
    ops.set_current_cpu(0);
    ops.set_line_enabled(13, Some(2), false);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);

    let handle = registry
        .request(
            irq(13),
            count_request(&counter)
                .scope(IrqScope::PerCpu {
                    cpus: CpuMask::from_cpu(CpuId(2)),
                })
                .auto_enable(AutoEnable::No),
        )
        .unwrap();
    ops.inner.cpu_sync_calls.store(0, Ordering::SeqCst);
    ops.clear_calls();

    ops.set_in_irq(true);
    assert_eq!(registry.enable(handle), Err(IrqError::InIrqContext));
    ops.set_in_irq(false);

    assert_eq!(ops.inner.cpu_sync_calls.load(Ordering::SeqCst), 0);
    assert!(!ops.calls().contains(&OpCall::SetEnabled {
        irq: 13,
        cpu: Some(2),
        enabled: true,
    }));
}

#[test]
fn failed_per_cpu_enable_rolls_back_action_state() {
    let ops = MockOps::with_cpus(4);
    ops.set_current_cpu(0);
    ops.set_line_enabled(18, Some(2), false);
    ops.fail_set_enabled(18, Some(2), true);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);

    let handle = registry
        .request(
            irq(18),
            count_request(&counter)
                .scope(IrqScope::PerCpu {
                    cpus: CpuMask::from_cpu(CpuId(2)),
                })
                .auto_enable(AutoEnable::No),
        )
        .unwrap();

    assert_eq!(registry.enable(handle), Err(IrqError::Controller));
    assert_eq!(registry.dispatch(irq(18), CpuId(2)).called, 0);
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

    let handle = registry
        .request(
            irq(13),
            count_request(&counter)
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
fn cpu_online_bookkeeping_is_rejected_from_irq_context() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());

    ops.set_in_irq(true);
    assert_eq!(registry.cpu_online(CpuId(0)), Err(IrqError::InIrqContext));
    ops.set_in_irq(false);
}

#[test]
fn pending_enable_is_tracked_per_cpu() {
    let ops = MockOps::with_cpus(4);
    ops.set_current_cpu(0);
    ops.set_online(2, false);
    ops.set_online(3, false);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);
    let mut cpus = CpuMask::empty();
    cpus.insert(CpuId(2));
    cpus.insert(CpuId(3));

    let handle = registry
        .request(
            irq(19),
            count_request(&counter)
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

    let handle = registry
        .request(
            irq(17),
            count_request(&counter)
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

    let handle = registry
        .request(irq(14), count_request(&counter).auto_enable(AutoEnable::No))
        .unwrap();

    let status = registry.status(handle).unwrap();
    assert!(!status.action_enabled);
    assert!(!status.line_enabled);
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

    let handle = registry
        .request(irq(20), count_request(&counter).auto_enable(AutoEnable::No))
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
                block_false_once: AtomicBool::new(false),
                line_enabled: AtomicBool::new(false),
                calls: Mutex::new(Vec::new()),
            }),
        }
    }

    fn block_next_disable(&self) {
        self.inner.block_false_once.store(true, Ordering::SeqCst);
    }
}

// SAFETY: This adapter never defers a CPU thunk; the unreachable method is not
// used by its global-line tests. All shared state is synchronized or atomic.
unsafe impl IrqOps for BlockingLineOps {
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

    fn set_enabled(&self, irq: IrqId, cpu: Option<CpuId>, enabled: bool) -> Result<(), IrqError> {
        let raw_irq = raw_irq(irq);
        self.inner.calls.lock().unwrap().push(OpCall::SetEnabled {
            irq: raw_irq,
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

    fn set_affinity(&self, irq: IrqId, affinity: IrqAffinity) -> Result<(), IrqError> {
        let raw_irq = raw_irq(irq);
        self.inner.calls.lock().unwrap().push(OpCall::SetAffinity {
            irq: raw_irq,
            affinity,
        });
        Ok(())
    }

    fn is_enabled(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn is_pending(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn is_in_service(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
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
        .request(irq(21), count_request(&first).share_mode(ShareMode::Shared))
        .unwrap();
    registry.enable(first).unwrap();
    let second = registry
        .request(
            irq(21),
            count_request(&second)
                .share_mode(ShareMode::Shared)
                .auto_enable(AutoEnable::No),
        )
        .unwrap();

    ops.block_next_disable();
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
        .request(irq(16), count_request(&first).share_mode(ShareMode::Shared))
        .unwrap();
    let second = registry
        .request(
            irq(16),
            count_request(&second).share_mode(ShareMode::Shared),
        )
        .unwrap();
    ops.clear_calls();

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

    registry
        .request(
            irq(15),
            wake_request(&counter).share_mode(ShareMode::Shared),
        )
        .unwrap();

    let outcome = registry.dispatch(irq(15), CpuId(0));
    assert!(outcome.handled);
    assert!(outcome.wake);
    assert_eq!(outcome.called, 1);
}

#[test]
fn free_from_irq_context_is_rejected() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let counter = AtomicUsize::new(0);
    let handle = registry
        .request(irq(16), count_request(&counter).auto_enable(AutoEnable::No))
        .unwrap();

    ops.set_in_irq(true);
    assert_eq!(registry.free(handle), Err(IrqError::InIrqContext));
    ops.set_in_irq(false);
    registry.free(handle).unwrap();
}

#[test]
fn shared_line_reopens_only_after_every_deferred_action_generation_finishes() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops.clone());
    let (first_capture, first_wake) = ContinuationCapture::allocate();
    let (second_capture, second_wake) = ContinuationCapture::allocate();

    let first = registry
        .request(
            irq(81),
            enabled_request(move |_| IrqReturn::Defer(first_wake))
                .share_mode(ShareMode::Shared),
        )
        .unwrap();
    let second = registry
        .request(
            irq(81),
            enabled_request(move |_| IrqReturn::Defer(second_wake))
                .share_mode(ShareMode::Shared),
        )
        .unwrap();

    let outcome = registry.dispatch(irq(81), CpuId(0));
    assert!(outcome.handled && outcome.wake);
    assert_eq!(first_capture.wakes.load(Ordering::Acquire), 1);
    assert_eq!(second_capture.wakes.load(Ordering::Acquire), 1);
    assert!(registry.status(first).unwrap().continuation_pending);
    assert!(registry.status(second).unwrap().continuation_pending);
    assert!(!registry.status(first).unwrap().line_enabled);

    registry
        .finish_continuation(first_capture.take())
        .unwrap();
    assert!(
        !registry.status(first).unwrap().line_enabled,
        "the second shared action still owns the masked line"
    );
    assert!(!registry.status(first).unwrap().continuation_pending);
    assert!(registry.status(second).unwrap().continuation_pending);

    registry
        .finish_continuation(second_capture.take())
        .unwrap();
    assert!(registry.status(first).unwrap().line_enabled);
    assert!(!registry.status(second).unwrap().continuation_pending);
}

#[test]
fn edge_replay_waits_for_the_exact_continuation_before_redispatch() {
    let ops = MockOps::with_cpus(1);
    let registry = Registry::new(ops);
    let (capture, wake) = ContinuationCapture::allocate();
    let calls: &'static AtomicUsize = Box::leak(Box::new(AtomicUsize::new(0)));
    let handle = registry
        .request(
            irq(82),
            enabled_request(move |_| {
                if calls.fetch_add(1, Ordering::AcqRel) == 0 {
                    IrqReturn::Defer(wake)
                } else {
                    IrqReturn::Handled
                }
            }),
        )
        .unwrap();

    assert!(registry.dispatch(irq(82), CpuId(0)).handled);
    let masked_arrival = registry.dispatch(irq(82), CpuId(0));
    assert_eq!(masked_arrival.called, 0);
    assert_eq!(calls.load(Ordering::Acquire), 1);

    registry.finish_continuation(capture.take()).unwrap();
    let replay = registry.dispatch(irq(82), CpuId(0));
    assert!(replay.handled);
    assert_eq!(calls.load(Ordering::Acquire), 2);
    assert!(!registry.status(handle).unwrap().continuation_pending);
}

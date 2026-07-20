//! Prepared IRQ-chip line endpoints.
//!
//! Controller discovery and route validation run once in task context. The
//! resulting endpoint is retained in a shutdown-lifetime arena, while the IRQ
//! framework carries only a generation-checked value key. Live mask/unmask
//! never re-enters the driver registry.

use alloc::{boxed::Box, vec::Vec};
use core::{
    pin::Pin,
    ptr,
    sync::atomic::{AtomicPtr, AtomicU8, Ordering},
};

use ax_kspin::SpinNoPreempt;
use irq_framework::{
    CpuId, IrqAffinity, IrqError, IrqId, IrqLineBinding, IrqLineControl, IrqScope, PreparedIrqLine,
};

use crate::{arch::Plat, common::PlatOp, irq::IrqAffinity as PlatformIrqAffinity};

const IRQ_LINE_CAPACITY: usize = 4096;
const INITIAL_GENERATION: u64 = 1;

/// A bounded snapshot exposed by a prepared IRQ-chip endpoint.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BoundIrqStatus {
    /// Whether the line is enabled, when the controller exposes that state.
    pub enabled: Option<bool>,
    /// Whether the line is pending, when the controller exposes that state.
    pub pending: Option<bool>,
    /// Whether the line is active/in-service, when the controller exposes that state.
    pub in_service: Option<bool>,
}

/// Stable controller capability prepared before an IRQ action is published.
///
/// # Safety
///
/// Implementors must retain every referenced register capability until
/// shutdown and must complete [`IrqChipLine::set_enabled`] without allocation,
/// freeing, blocking, generic-device lookup, arbitrary callbacks, or an
/// unbounded wait. After successful preparation, every valid scope/CPU
/// transition is infallible; lost hardware ownership is a fatal platform
/// invariant rather than a recoverable live-path error.
pub(crate) unsafe trait IrqChipLine: Send + Sync + 'static {
    /// Changes the enable state after all fallible validation has completed.
    fn set_enabled(&self, cpu: Option<CpuId>, enabled: bool);

    /// Reads bounded controller state without allocating or blocking.
    fn status(&self, _cpu: Option<CpuId>) -> BoundIrqStatus {
        BoundIrqStatus::default()
    }

    /// Releases the endpoint's controller ownership while it remains pinned.
    ///
    /// A failed release must leave the endpoint usable with the same binding.
    /// Platforms that retain this kind of endpoint until shutdown may keep the
    /// default unsupported result.
    fn release(&self) -> Result<(), IrqError> {
        Err(IrqError::Unsupported)
    }
}

pub(crate) type BoxedIrqChipLine = Box<dyn IrqChipLine>;

/// Fully validated platform result before it enters the value-binding arena.
pub(crate) struct PreparedIrqChipLine {
    control: IrqLineControl,
    endpoint: BoxedIrqChipLine,
}

impl PreparedIrqChipLine {
    pub(crate) fn maskable(endpoint: BoxedIrqChipLine) -> Self {
        Self {
            control: IrqLineControl::Maskable,
            endpoint,
        }
    }

    #[cfg(target_arch = "x86_64")]
    pub(crate) fn action_gate_only() -> Self {
        Self {
            control: IrqLineControl::ActionGateOnly,
            endpoint: Box::new(ActionGateOnlyLine),
        }
    }
}

#[cfg(target_arch = "x86_64")]
struct ActionGateOnlyLine;

// SAFETY: this endpoint owns no hardware capability. The framework must never
// invoke a physical transition for ActionGateOnly; doing so is a fatal typed
// control-mode violation rather than a fallible live operation.
#[cfg(target_arch = "x86_64")]
unsafe impl IrqChipLine for ActionGateOnlyLine {
    fn set_enabled(&self, _cpu: Option<CpuId>, _enabled: bool) {
        panic!("fatal platform invariant: ActionGateOnly IRQ reached irq-chip line control")
    }
}

/// Resolves and permanently retains one controller line.
pub fn prepare_irq_line(
    irq: IrqId,
    scope: IrqScope,
    affinity: IrqAffinity,
) -> Result<PreparedIrqLine, IrqError> {
    let platform_affinity = match affinity {
        IrqAffinity::Any => PlatformIrqAffinity::Any,
        IrqAffinity::Fixed(cpu) => PlatformIrqAffinity::Fixed { cpu_id: cpu.0 },
    };
    IRQ_LINES.prepare(irq, scope, affinity, || {
        Plat::prepare_irq_line(irq, scope, platform_affinity)
    })
}

/// Applies an infallible live transition through a prepared value binding.
pub fn set_bound_irq_enabled(binding: IrqLineBinding, cpu: Option<CpuId>, enabled: bool) {
    IRQ_LINES
        .bound_line(binding)
        .endpoint
        .set_enabled(cpu, enabled);
}

/// Releases one exact prepared line generation.
///
/// The endpoint remains pinned after success, but its arena slot becomes
/// reusable. A future preparation receives a fresh generation, so stale value
/// bindings cannot operate on the replacement.
pub fn release_irq_line(binding: IrqLineBinding) -> Result<(), IrqError> {
    IRQ_LINES.release(binding)
}

/// Reads a prepared endpoint without resolving the controller again.
pub fn bound_irq_status(irq: IrqId, cpu: Option<CpuId>) -> Result<BoundIrqStatus, IrqError> {
    IRQ_LINES
        .line_for_irq(irq)
        .map(|line| line.endpoint.status(cpu))
        .ok_or(IrqError::NotFound)
}

struct BoundLine {
    irq: IrqId,
    scope: IrqScope,
    affinity: IrqAffinity,
    generation: u64,
    control: IrqLineControl,
    endpoint: BoxedIrqChipLine,
    phase: AtomicU8,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundLinePhase {
    Active    = 0,
    Releasing = 1,
    Retired   = 2,
}

impl BoundLine {
    fn phase(&self) -> BoundLinePhase {
        match self.phase.load(Ordering::Acquire) {
            0 => BoundLinePhase::Active,
            1 => BoundLinePhase::Releasing,
            2 => BoundLinePhase::Retired,
            value => panic!("fatal platform invariant: invalid IRQ line phase {value}"),
        }
    }

    fn set_phase(&self, phase: BoundLinePhase) {
        self.phase.store(phase as u8, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingLine {
    slot: usize,
    irq: IrqId,
    scope: IrqScope,
    affinity: IrqAffinity,
    generation: u64,
}

struct LineArenaState {
    retained: Vec<Pin<Box<BoundLine>>>,
    pending: Vec<PendingLine>,
    next_generation: u64,
}

impl LineArenaState {
    const fn new() -> Self {
        Self {
            retained: Vec::new(),
            pending: Vec::new(),
            next_generation: INITIAL_GENERATION,
        }
    }

    fn take_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = generation
            .checked_add(1)
            .expect("fatal platform invariant: IRQ line generation exhausted");
        generation
    }
}

struct IrqLineArena<const N: usize> {
    slots: [AtomicPtr<BoundLine>; N],
    state: SpinNoPreempt<LineArenaState>,
}

impl<const N: usize> IrqLineArena<N> {
    const fn new() -> Self {
        Self {
            slots: [const { AtomicPtr::new(ptr::null_mut()) }; N],
            state: SpinNoPreempt::new(LineArenaState::new()),
        }
    }

    fn prepare(
        &self,
        irq: IrqId,
        scope: IrqScope,
        affinity: IrqAffinity,
        make_endpoint: impl FnOnce() -> Result<PreparedIrqChipLine, IrqError>,
    ) -> Result<PreparedIrqLine, IrqError> {
        let pending = {
            let mut state = self.state.lock();
            if let Some((slot, line)) = self.find_line(irq) {
                if line.phase() != BoundLinePhase::Active {
                    return Err(IrqError::Busy);
                }
                if line.scope != scope || line.affinity != affinity {
                    return Err(IrqError::Busy);
                }
                return Ok(PreparedIrqLine::new(
                    binding(slot, line.generation),
                    line.control,
                ));
            }
            if state.pending.iter().any(|pending| pending.irq == irq) {
                return Err(IrqError::Busy);
            }

            let slot = self
                .vacant_slot(irq, &state.pending)
                .ok_or(IrqError::NoMemory)?;
            // Capacity covers every outstanding reservation, so committing a
            // platform endpoint cannot perform fallible arena growth after the
            // controller source has been reserved and masked.
            let outstanding = state.pending.len();
            state
                .retained
                .try_reserve(outstanding + 1)
                .map_err(|_| IrqError::NoMemory)?;
            state
                .pending
                .try_reserve(1)
                .map_err(|_| IrqError::NoMemory)?;
            let pending = PendingLine {
                slot,
                irq,
                scope,
                affinity,
                generation: state.take_generation(),
            };
            state.pending.push(pending);
            pending
        };

        // Controller discovery and route reservation can take driver-owned
        // control-plane locks. It must never run under the arena lock.
        let prepared = match make_endpoint() {
            Ok(prepared) => prepared,
            Err(error) => {
                self.cancel_pending(pending);
                return Err(error);
            }
        };
        let control = prepared.control;
        let line = Box::pin(BoundLine {
            irq,
            scope,
            affinity,
            generation: pending.generation,
            control,
            endpoint: prepared.endpoint,
            phase: AtomicU8::new(BoundLinePhase::Active as u8),
        });
        let line_ptr = Pin::as_ref(&line).get_ref() as *const BoundLine as *mut BoundLine;

        let mut state = self.state.lock();
        let pending_index = state
            .pending
            .iter()
            .position(|candidate| *candidate == pending)
            .unwrap_or_else(|| fatal_pending(pending));
        if !self.slots[pending.slot].load(Ordering::Acquire).is_null() {
            fatal_pending(pending);
        }
        state.retained.push(line);
        self.slots[pending.slot].store(line_ptr, Ordering::Release);
        state.pending.swap_remove(pending_index);
        Ok(PreparedIrqLine::new(
            binding(pending.slot, pending.generation),
            control,
        ))
    }

    fn cancel_pending(&self, pending: PendingLine) {
        let mut state = self.state.lock();
        let pending_index = state
            .pending
            .iter()
            .position(|candidate| *candidate == pending)
            .unwrap_or_else(|| fatal_pending(pending));
        state.pending.swap_remove(pending_index);
    }

    fn bound_line(&self, binding: IrqLineBinding) -> &BoundLine {
        let slot = usize::try_from(binding.slot())
            .ok()
            .filter(|slot| *slot < N)
            .unwrap_or_else(|| fatal_binding(binding));
        let line = self.slots[slot].load(Ordering::Acquire);
        if line.is_null() {
            fatal_binding(binding);
        }
        // SAFETY: slots are published only after their boxed line enters the
        // shutdown-lifetime arena. Entries are never replaced or released.
        let line = unsafe { &*line };
        if line.generation != binding.generation() {
            fatal_binding(binding);
        }
        if line.phase() != BoundLinePhase::Active {
            fatal_binding(binding);
        }
        line
    }

    fn release(&self, binding: IrqLineBinding) -> Result<(), IrqError> {
        let (slot, line) = {
            let _state = self.state.lock();
            let slot = usize::try_from(binding.slot())
                .ok()
                .filter(|slot| *slot < N)
                .ok_or(IrqError::NotFound)?;
            let line = self.slots[slot].load(Ordering::Acquire);
            if line.is_null() {
                return Err(IrqError::NotFound);
            }
            let line = unsafe {
                // SAFETY: a published slot points into the pinned retained
                // arena, which never reclaims retired endpoint objects.
                &*line
            };
            if line.generation != binding.generation() {
                return Err(IrqError::NotFound);
            }
            if line.phase() != BoundLinePhase::Active {
                return Err(IrqError::Busy);
            }
            line.set_phase(BoundLinePhase::Releasing);
            (slot, line)
        };

        // Controller synchronization may take a driver control-plane lock and
        // must not serialize unrelated arena reservations.
        let result = line.endpoint.release();

        let _state = self.state.lock();
        let current = self.slots[slot].load(Ordering::Acquire);
        assert_eq!(
            current, line as *const BoundLine as *mut BoundLine,
            "IRQ line release lost its reserved arena slot"
        );
        assert_eq!(
            line.phase(),
            BoundLinePhase::Releasing,
            "IRQ line release lost its arena phase"
        );
        match result {
            Ok(()) => {
                line.set_phase(BoundLinePhase::Retired);
                self.slots[slot].store(ptr::null_mut(), Ordering::Release);
                Ok(())
            }
            Err(error) => {
                line.set_phase(BoundLinePhase::Active);
                Err(error)
            }
        }
    }

    fn line_for_irq(&self, irq: IrqId) -> Option<&BoundLine> {
        self.find_line(irq)
            .map(|(_, line)| line)
            .filter(|line| line.phase() == BoundLinePhase::Active)
    }

    fn find_line(&self, irq: IrqId) -> Option<(usize, &BoundLine)> {
        let start = line_hash(irq) % N;
        for distance in 0..N {
            let slot = (start + distance) % N;
            let line = self.slots[slot].load(Ordering::Acquire);
            if line.is_null() {
                continue;
            }
            // SAFETY: a non-null slot points into the shutdown-lifetime arena
            // and publication is observed through the Acquire load above.
            let line = unsafe { &*line };
            if line.irq == irq {
                return Some((slot, line));
            }
        }
        None
    }

    fn vacant_slot(&self, irq: IrqId, pending: &[PendingLine]) -> Option<usize> {
        let start = line_hash(irq) % N;
        (0..N).map(|distance| (start + distance) % N).find(|slot| {
            self.slots[*slot].load(Ordering::Acquire).is_null()
                && pending.iter().all(|reservation| reservation.slot != *slot)
        })
    }
}

static IRQ_LINES: IrqLineArena<IRQ_LINE_CAPACITY> = IrqLineArena::new();

fn line_hash(irq: IrqId) -> usize {
    let key = (u64::from(irq.domain.0) << 32) | u64::from(irq.hwirq.0);
    let mixed = key.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    (mixed ^ (mixed >> 32)) as usize
}

fn binding(slot: usize, generation: u64) -> IrqLineBinding {
    let slot = u32::try_from(slot).expect("IRQ line arena slot must fit in u32");
    IrqLineBinding::new(slot, generation).expect("IRQ line generation must be nonzero")
}

#[cold]
fn fatal_binding(binding: IrqLineBinding) -> ! {
    panic!("fatal platform invariant: stale IRQ line binding {binding:?}")
}

#[cold]
fn fatal_pending(pending: PendingLine) -> ! {
    panic!("fatal platform invariant: lost IRQ line reservation {pending:?}")
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, AtomicUsize};
    use std::{panic::AssertUnwindSafe, sync::Barrier, thread};

    use irq_framework::{CpuMask, HwIrq, IrqDomainId};

    use super::*;

    struct TestLine {
        enabled: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
    }

    // SAFETY: this in-memory test endpoint is retained by the local arena and
    // its atomic operations are bounded and allocation-free.
    unsafe impl IrqChipLine for TestLine {
        fn set_enabled(&self, _cpu: Option<CpuId>, enabled: bool) {
            self.enabled.store(enabled, Ordering::Release);
            self.calls.fetch_add(1, Ordering::Relaxed);
        }

        fn status(&self, _cpu: Option<CpuId>) -> BoundIrqStatus {
            BoundIrqStatus {
                enabled: Some(self.enabled.load(Ordering::Acquire)),
                ..BoundIrqStatus::default()
            }
        }
    }

    struct ReleasableTestLine {
        release_calls: Arc<AtomicUsize>,
        fail_release: bool,
        release_entered: Option<Arc<Barrier>>,
        release_resume: Option<Arc<Barrier>>,
    }

    // SAFETY: release only touches test atomics/barriers. Test barriers provide
    // a bounded rendezvous and are never used by a hard-IRQ live transition.
    unsafe impl IrqChipLine for ReleasableTestLine {
        fn set_enabled(&self, _cpu: Option<CpuId>, _enabled: bool) {}

        fn release(&self) -> Result<(), IrqError> {
            self.release_calls.fetch_add(1, Ordering::Relaxed);
            if let Some(entered) = &self.release_entered {
                entered.wait();
            }
            if let Some(resume) = &self.release_resume {
                resume.wait();
            }
            if self.fail_release {
                Err(IrqError::Controller)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn prepared_binding_reuses_the_same_stable_endpoint() {
        let arena = IrqLineArena::<4>::new();
        let irq = IrqId::new(IrqDomainId(7), HwIrq(11));
        let enabled = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let make_calls = AtomicUsize::new(0);
        let binding = arena
            .prepare(irq, IrqScope::Global, IrqAffinity::Any, || {
                make_calls.fetch_add(1, Ordering::Relaxed);
                Ok(PreparedIrqChipLine::maskable(Box::new(TestLine {
                    enabled: enabled.clone(),
                    calls: calls.clone(),
                })))
            })
            .unwrap();
        let reused = arena
            .prepare(irq, IrqScope::Global, IrqAffinity::Any, || {
                make_calls.fetch_add(1, Ordering::Relaxed);
                Err(IrqError::Controller)
            })
            .unwrap();

        assert_eq!(binding, reused);
        assert_eq!(make_calls.load(Ordering::Relaxed), 1);
        arena
            .bound_line(binding.binding())
            .endpoint
            .set_enabled(None, true);
        assert!(enabled.load(Ordering::Acquire));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn prepared_binding_rejects_a_different_route_contract() {
        let arena = IrqLineArena::<4>::new();
        let irq = IrqId::new(IrqDomainId(7), HwIrq(12));
        let make = || {
            Ok(PreparedIrqChipLine::maskable(Box::new(TestLine {
                enabled: Arc::new(AtomicBool::new(false)),
                calls: Arc::new(AtomicUsize::new(0)),
            })
                as BoxedIrqChipLine))
        };
        arena
            .prepare(irq, IrqScope::Global, IrqAffinity::Any, make)
            .unwrap();

        let error = arena
            .prepare(
                irq,
                IrqScope::PerCpu {
                    cpus: CpuMask::from_cpu(CpuId(0)),
                },
                IrqAffinity::Any,
                make,
            )
            .unwrap_err();

        assert_eq!(error, IrqError::Busy);
    }

    #[test]
    fn released_slot_is_reused_only_with_a_fresh_generation() {
        let arena = IrqLineArena::<4>::new();
        let irq = IrqId::new(IrqDomainId(7), HwIrq(14));
        let release_calls = Arc::new(AtomicUsize::new(0));
        let make_line = || {
            Ok(PreparedIrqChipLine::maskable(Box::new(
                ReleasableTestLine {
                    release_calls: Arc::clone(&release_calls),
                    fail_release: false,
                    release_entered: None,
                    release_resume: None,
                },
            )))
        };
        let first = arena
            .prepare(irq, IrqScope::Global, IrqAffinity::Any, make_line)
            .unwrap();

        arena.release(first.binding()).unwrap();

        assert_eq!(release_calls.load(Ordering::Relaxed), 1);
        assert_eq!(arena.release(first.binding()), Err(IrqError::NotFound));
        assert!(
            std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _ = arena.bound_line(first.binding());
            }))
            .is_err(),
            "a retired generation must be rejected by the live binding path"
        );
        let replacement = arena
            .prepare(irq, IrqScope::Global, IrqAffinity::Any, make_line)
            .unwrap();
        assert_eq!(replacement.binding().slot(), first.binding().slot());
        assert_ne!(
            replacement.binding().generation(),
            first.binding().generation()
        );
    }

    #[test]
    fn failed_release_restores_the_active_binding() {
        let arena = IrqLineArena::<4>::new();
        let irq = IrqId::new(IrqDomainId(7), HwIrq(15));
        let release_calls = Arc::new(AtomicUsize::new(0));
        let prepared = arena
            .prepare(irq, IrqScope::Global, IrqAffinity::Any, || {
                Ok(PreparedIrqChipLine::maskable(Box::new(
                    ReleasableTestLine {
                        release_calls: Arc::clone(&release_calls),
                        fail_release: true,
                        release_entered: None,
                        release_resume: None,
                    },
                )))
            })
            .unwrap();

        assert_eq!(arena.release(prepared.binding()), Err(IrqError::Controller));

        assert_eq!(release_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            arena
                .prepare(irq, IrqScope::Global, IrqAffinity::Any, || {
                    Err(IrqError::Controller)
                })
                .unwrap(),
            prepared
        );
        let _ = arena.bound_line(prepared.binding());
    }

    #[test]
    fn racing_prepare_observes_releasing_and_controller_work_runs_unlocked() {
        let arena = Arc::new(IrqLineArena::<4>::new());
        let irq = IrqId::new(IrqDomainId(7), HwIrq(16));
        let release_entered = Arc::new(Barrier::new(2));
        let release_resume = Arc::new(Barrier::new(2));
        let prepared = arena
            .prepare(irq, IrqScope::Global, IrqAffinity::Any, || {
                Ok(PreparedIrqChipLine::maskable(Box::new(
                    ReleasableTestLine {
                        release_calls: Arc::new(AtomicUsize::new(0)),
                        fail_release: false,
                        release_entered: Some(Arc::clone(&release_entered)),
                        release_resume: Some(Arc::clone(&release_resume)),
                    },
                )))
            })
            .unwrap();
        let releasing_arena = Arc::clone(&arena);
        let releasing = thread::spawn(move || releasing_arena.release(prepared.binding()));
        release_entered.wait();

        assert!(
            arena.state.try_lock().is_some(),
            "controller release must not retain the IRQ line arena lock"
        );
        assert_eq!(
            arena.prepare(irq, IrqScope::Global, IrqAffinity::Any, || {
                Err(IrqError::Controller)
            }),
            Err(IrqError::Busy)
        );

        release_resume.wait();
        assert_eq!(releasing.join().unwrap(), Ok(()));
    }

    #[test]
    fn platform_factory_runs_outside_the_arena_lock() {
        let arena = IrqLineArena::<4>::new();
        let irq = IrqId::new(IrqDomainId(7), HwIrq(13));
        let factory_observed_unlocked = AtomicBool::new(false);

        arena
            .prepare(irq, IrqScope::Global, IrqAffinity::Any, || {
                factory_observed_unlocked
                    .store(arena.state.try_lock().is_some(), Ordering::Relaxed);
                Ok(PreparedIrqChipLine::maskable(Box::new(TestLine {
                    enabled: Arc::new(AtomicBool::new(false)),
                    calls: Arc::new(AtomicUsize::new(0)),
                })))
            })
            .unwrap();

        assert!(factory_observed_unlocked.load(Ordering::Relaxed));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn action_gate_control_survives_arena_reuse() {
        let arena = IrqLineArena::<4>::new();
        let irq = IrqId::new(IrqDomainId(9), HwIrq(3));
        let scope = IrqScope::PerCpu {
            cpus: CpuMask::from_cpu(CpuId(0)),
        };

        let prepared = arena
            .prepare(irq, scope, IrqAffinity::Any, || {
                Ok(PreparedIrqChipLine::action_gate_only())
            })
            .unwrap();
        let reused = arena
            .prepare(irq, scope, IrqAffinity::Any, || Err(IrqError::Controller))
            .unwrap();

        assert_eq!(prepared, reused);
        assert_eq!(prepared.control(), IrqLineControl::ActionGateOnly);
    }
}

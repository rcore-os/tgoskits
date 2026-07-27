use alloc::{
    alloc::{alloc, dealloc, handle_alloc_error},
    boxed::Box,
};
use core::{
    alloc::Layout,
    cell::UnsafeCell,
    iter::zip,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_kernel_guard::NoPreempt;
use ax_percpu::CpuPin;
use spin::Once;

use crate::{
    boxed::ItemBox,
    item::{Item, Registry},
};

const SCOPE_GATE_WRITER: usize = 1 << (usize::BITS - 1);
const SCOPE_GATE_READERS: usize = SCOPE_GATE_WRITER - 1;

/// Writer-preferred raw gate for scheduler-owned scope leases.
///
/// The writer bit is published before an exclusive caller waits for existing
/// readers to drain. New activations therefore cannot barge ahead of a pending
/// writer and keep task-context mutation spinning indefinitely.
struct ScopeGate {
    state: AtomicUsize,
}

impl ScopeGate {
    const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
        }
    }

    fn lock_shared(&self) {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state & SCOPE_GATE_WRITER != 0 {
                core::hint::spin_loop();
                state = self.state.load(Ordering::Acquire);
                continue;
            }
            assert_ne!(
                state & SCOPE_GATE_READERS,
                SCOPE_GATE_READERS,
                "scope shared lease count overflow"
            );
            match self.state.compare_exchange_weak(
                state,
                state + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(observed) => state = observed,
            }
        }
    }

    fn try_lock_shared(&self) -> bool {
        let state = self.state.load(Ordering::Acquire);
        if state & SCOPE_GATE_WRITER != 0 || state & SCOPE_GATE_READERS == SCOPE_GATE_READERS {
            return false;
        }
        self.state
            .compare_exchange(state, state + 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn lock_exclusive_writer_preferred(&self) {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state & SCOPE_GATE_WRITER != 0 {
                core::hint::spin_loop();
                state = self.state.load(Ordering::Acquire);
                continue;
            }
            match self.state.compare_exchange_weak(
                state,
                state | SCOPE_GATE_WRITER,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => state = observed,
            }
        }
        while self.state.load(Ordering::Acquire) != SCOPE_GATE_WRITER {
            core::hint::spin_loop();
        }
    }

    unsafe fn unlock_shared(&self) {
        let old = self.state.fetch_sub(1, Ordering::Release);
        assert_ne!(
            old & SCOPE_GATE_READERS,
            0,
            "scope shared unlock without a matching lease"
        );
    }

    unsafe fn unlock_exclusive(&self) {
        assert_eq!(
            self.state.load(Ordering::Relaxed),
            SCOPE_GATE_WRITER,
            "scope exclusive unlock with live readers or no writer"
        );
        self.state.store(0, Ordering::Release);
    }

    fn is_locked(&self) -> bool {
        self.state.load(Ordering::Acquire) != 0
    }
}

#[cfg(test)]
mod scope_gate_tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };

    use super::{SCOPE_GATE_WRITER, ScopeGate};

    #[test]
    fn pending_writer_prevents_new_reader_barging() {
        let gate = Arc::new(ScopeGate::new());
        let writer_acquired = Arc::new(AtomicBool::new(false));
        gate.lock_shared();

        let writer_gate = gate.clone();
        let writer_state = writer_acquired.clone();
        let writer = thread::spawn(move || {
            writer_gate.lock_exclusive_writer_preferred();
            writer_state.store(true, Ordering::Release);
            unsafe { writer_gate.unlock_exclusive() };
        });

        while gate.state.load(Ordering::Acquire) & SCOPE_GATE_WRITER == 0 {
            thread::yield_now();
        }
        assert!(!gate.try_lock_shared());
        assert!(!writer_acquired.load(Ordering::Acquire));

        unsafe { gate.unlock_shared() };
        writer.join().unwrap();
        assert!(writer_acquired.load(Ordering::Acquire));
        assert!(!gate.is_locked());
    }
}

/// A scope is a collection of items.
pub struct Scope {
    inner: Box<ScopeInner>,
}

struct ScopeInner {
    gate: ScopeGate,
    slots: NonNull<UnsafeCell<ItemSlot>>,
}

// SAFETY: the public registration path admits only `Send + Sync + 'static`
// payloads, and ownership of every initialized slot moves with the Scope.
unsafe impl Send for Scope {}
// SAFETY: shared payload access is admitted only for `Sync` values and is
// serialized against mutation by `gate`.
unsafe impl Sync for Scope {}

impl Scope {
    /// Creates a new namespace and eagerly initializes every registered item.
    ///
    /// Initializers run in the caller's ordinary context. Once this function
    /// returns, pinned access to the scope performs no allocation or lazy
    /// initialization.
    pub fn new() -> Self {
        Self {
            inner: Box::new(ScopeInner::new()),
        }
    }

    fn inner(&self) -> &ScopeInner {
        &self.inner
    }

    fn inner_ptr(&self) -> *const ScopeInner {
        self.inner.as_ref()
    }

    pub(crate) fn read_item(&self, item: &'static Item) -> ScopeItemLease<'_> {
        self.inner.read_item(item)
    }

    pub(crate) fn get_mut_unlocked(&mut self, item: &'static Item) -> &mut ItemBox {
        // SAFETY: `&mut Scope` gives exclusive ownership of this namespace and
        // therefore of the selected UnsafeCell-backed slot.
        unsafe { (&mut *self.inner.slot_ptr(item)).get_mut() }
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeInner {
    fn len() -> usize {
        Registry.len()
    }

    fn layout() -> Layout {
        Layout::array::<UnsafeCell<ItemSlot>>(Self::len()).unwrap()
    }

    fn new() -> Self {
        let layout = Self::layout();
        let ptr = NonNull::new(unsafe { alloc(layout) })
            .unwrap_or_else(|| handle_alloc_error(layout))
            .cast();

        let slice = unsafe {
            core::slice::from_raw_parts_mut(ptr.cast::<MaybeUninit<_>>().as_ptr(), Registry.len())
        };
        for (item, d) in zip(&*Registry, slice) {
            d.write(UnsafeCell::new(ItemSlot::new(item)));
        }

        Self {
            gate: ScopeGate::new(),
            slots: ptr,
        }
    }

    fn lock_shared(&self) {
        self.gate.lock_shared();
    }

    fn lock_exclusive_writer_preferred(&self) {
        self.gate.lock_exclusive_writer_preferred();
    }

    unsafe fn unlock_shared(&self) {
        // SAFETY: forwarded to the caller, which must own one shared count.
        unsafe { self.gate.unlock_shared() };
    }

    unsafe fn unlock_exclusive(&self) {
        // SAFETY: forwarded to the caller, which must own the exclusive count.
        unsafe { self.gate.unlock_exclusive() };
    }

    pub(crate) fn read_item(&self, item: &'static Item) -> ScopeItemLease<'_> {
        self.lock_shared();
        ScopeItemLease { inner: self, item }
    }

    fn get_shared(&self, item: &'static Item) -> &ItemBox {
        let index = item.index();
        // SAFETY: callers either own a shared gate count or access the
        // immutable global scope. Both cases exclude the only mutable path.
        unsafe { (&*self.slots.add(index).as_ref().get()).get() }
    }

    fn try_get_shared(&self, item: &'static Item) -> Option<&ItemBox> {
        let index = item.index();
        // SAFETY: the same shared-lease or immutable-global invariant as
        // `get_shared` applies.
        unsafe { (&*self.slots.add(index).as_ref().get()).try_get() }
    }

    fn slot_ptr(&self, item: &'static Item) -> *mut ItemSlot {
        let index = item.index();
        // SAFETY: address calculation does not create a reference. Callers must
        // own either `&mut Scope` or the exclusive gate before dereferencing.
        unsafe { self.slots.add(index).as_ref().get() }
    }
}

pub(crate) struct ScopeItemLease<'scope> {
    inner: &'scope ScopeInner,
    item: &'static Item,
}

impl ScopeItemLease<'_> {
    pub(crate) fn item(&self) -> &ItemBox {
        self.inner.get_shared(self.item)
    }
}

impl Drop for ScopeItemLease<'_> {
    fn drop(&mut self) {
        // SAFETY: construction acquired exactly one shared count.
        unsafe { self.inner.unlock_shared() };
    }
}

impl Drop for ScopeInner {
    fn drop(&mut self) {
        let ptr = NonNull::slice_from_raw_parts(self.slots, Self::len());
        unsafe {
            ptr.drop_in_place();
            dealloc(self.slots.cast().as_ptr(), Self::layout());
        }
    }
}

/// A scope whose scheduler binding owns one shared lease while active.
///
/// Scheduler hooks acquire the lease before publishing the pinned pointer and
/// release it after clearing that pointer. Scope-local hot reads therefore need
/// no per-access lock. Writers use an upgradable lease to publish writer intent
/// before waiting for active tasks and bounded remote readers to drain.
pub struct ScopeCell {
    scope: Scope,
    active_cpus: AtomicUsize,
}

impl ScopeCell {
    /// Creates a managed scope with no active scheduler binding.
    pub fn new() -> Self {
        Self::from_scope(Scope::new())
    }

    /// Wraps an existing scope with a managed scheduler binding.
    pub fn from_scope(scope: Scope) -> Self {
        Self {
            scope,
            active_cpus: AtomicUsize::new(0),
        }
    }

    /// Acquires an ordinary shared scope reference while preventing migration.
    pub fn read(&self) -> ScopeCellReadGuard<'_> {
        let preempt = NoPreempt::new();
        self.scope.inner().lock_shared();
        ScopeCellReadGuard {
            scope: &self.scope,
            _preempt: preempt,
        }
    }

    /// Attempts to acquire an ordinary shared scope reference while preventing
    /// migration.
    pub fn try_read(&self) -> Option<ScopeCellReadGuard<'_>> {
        let preempt = NoPreempt::new();
        if !self.scope.inner().gate.try_lock_shared() {
            return None;
        }
        Some(ScopeCellReadGuard {
            scope: &self.scope,
            _preempt: preempt,
        })
    }

    /// Acquires an ordinary exclusive scope reference while preventing
    /// migration.
    pub fn write(&self) -> ScopeCellWriteGuard<'_> {
        let preempt = NoPreempt::new();
        let inner = self.scope.inner();
        inner.lock_exclusive_writer_preferred();
        ScopeCellWriteGuard {
            inner,
            _preempt: Some(preempt),
        }
    }

    /// Installs this scope for the pinned CPU and retains one shared lease.
    ///
    /// This operation does not enter a new IRQ or preemption context, so it is
    /// suitable for a scheduler switch-in hook that already owns its CPU-local
    /// baton.
    ///
    /// # Safety
    ///
    /// The caller must keep this `ScopeCell` alive, retain the current CPU pin,
    /// and invoke [`deactivate_pinned`](Self::deactivate_pinned) exactly once
    /// before another scope is installed or the cell can be dropped. The
    /// scheduler must run both hooks while holding its switch baton.
    pub unsafe fn activate_pinned(&self, pin: &CpuPin<'_>) {
        assert_eq!(
            ActiveScope::current_scope_ptr_pinned(pin),
            0,
            "scope activation requires the global scope to be current"
        );
        self.acquire_active_lease();
        // SAFETY: the caller contract keeps the cell live until deactivation,
        // and the retained shared lease excludes slot mutation.
        unsafe { ActiveScope::set_pinned(&self.scope, pin) };
    }

    /// Clears this scope from the pinned CPU and retires its active identity.
    ///
    /// # Safety
    ///
    /// The current CPU must own exactly one activation previously established
    /// by [`activate_pinned`](Self::activate_pinned) for this cell.
    pub unsafe fn deactivate_pinned(&self, pin: &CpuPin<'_>) {
        assert_eq!(
            ActiveScope::current_scope_ptr_pinned(pin),
            self.scope_ptr(),
            "scope deactivation does not match the active scope"
        );
        // SAFETY: the caller owns this managed activation and therefore may
        // clear its raw per-CPU pointer.
        unsafe { ActiveScope::set_global_pinned(pin) };
        self.release_active_lease();
    }

    /// Mutates the calling task's active scope.
    ///
    /// The active shared lease is withdrawn before the writer gate is acquired
    /// and restored before this function returns. This preserves lock-free
    /// scope-local reads during ordinary execution without carrying a Rust
    /// guard or preemption token through a context switch.
    ///
    /// # Safety
    ///
    /// This cell must have exactly one activation, owned by the current CPU,
    /// and `pin` must remain valid for the complete call. The caller must
    /// prevent reentrant scope-local access while `operation` runs.
    pub unsafe fn with_active_mut_pinned<R>(
        &self,
        pin: &CpuPin<'_>,
        operation: impl for<'scope> FnOnce(&'scope mut ScopeCellWriteGuard<'_>) -> R,
    ) -> R {
        assert_eq!(
            ActiveScope::current_scope_ptr_pinned(pin),
            self.scope_ptr(),
            "active scope mutation does not match the current scope"
        );
        assert_eq!(
            self.active_cpus.load(Ordering::Acquire),
            1,
            "active scope mutation requires exclusive scheduler ownership"
        );
        // SAFETY: the caller owns the sole activation verified above.
        unsafe { ActiveScope::set_global_pinned(pin) };
        self.release_active_lease();

        let inner = self.scope.inner();
        inner.lock_exclusive_writer_preferred();
        let mut mutation = ActiveScopeMutation {
            cell: self,
            pin,
            writer: Some(ScopeCellWriteGuard {
                inner,
                _preempt: None,
            }),
        };
        let result = operation(mutation.writer());
        drop(mutation);
        result
    }

    fn scope_ptr(&self) -> usize {
        self.scope.inner_ptr().expose_provenance()
    }

    fn acquire_active_lease(&self) {
        let inner = self.scope.inner();
        inner.lock_shared();
        if self
            .active_cpus
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .is_err()
        {
            // SAFETY: this function acquired exactly one shared count above.
            unsafe { inner.unlock_shared() };
            panic!("scope activation count overflow");
        }
    }

    fn release_active_lease(&self) {
        self.active_cpus
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .expect("scope deactivation without a matching activation");
        // SAFETY: every active identity owns exactly one shared count.
        unsafe { self.scope.inner().unlock_shared() };
    }
}

struct ActiveScopeMutation<'cell, 'pin_ref, 'cpu> {
    cell: &'cell ScopeCell,
    pin: &'pin_ref CpuPin<'cpu>,
    writer: Option<ScopeCellWriteGuard<'cell>>,
}

impl<'cell> ActiveScopeMutation<'cell, '_, '_> {
    fn writer(&mut self) -> &mut ScopeCellWriteGuard<'cell> {
        self.writer
            .as_mut()
            .expect("active scope mutation writer must be present")
    }
}

impl Drop for ActiveScopeMutation<'_, '_, '_> {
    fn drop(&mut self) {
        drop(self.writer.take());
        self.cell.acquire_active_lease();
        // SAFETY: construction withdrew this cell's sole activation on the
        // same pinned CPU, and the shared lease has now been restored.
        unsafe { ActiveScope::set_pinned(&self.cell.scope, self.pin) };
    }
}

impl Default for ScopeCell {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScopeCell {
    fn drop(&mut self) {
        assert_eq!(
            self.active_cpus.load(Ordering::Acquire),
            0,
            "cannot drop a scope with live scheduler activations"
        );
        assert!(
            !self.scope.inner().gate.is_locked(),
            "cannot drop a locked scope"
        );
    }
}

/// Shared ordinary-access guard returned by [`ScopeCell::read`].
pub struct ScopeCellReadGuard<'a> {
    scope: &'a Scope,
    _preempt: NoPreempt,
}

impl ScopeCellReadGuard<'_> {
    pub(crate) fn get(&self, item: &'static Item) -> &ItemBox {
        // This guard already owns the shared count. Reacquiring it here could
        // deadlock behind a pending upgradable writer while retaining the
        // original count.
        self.scope.inner().get_shared(item)
    }
}

impl Drop for ScopeCellReadGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: this guard owns one raw shared count. Its preemption guard is
        // dropped afterwards, preserving raw unlock -> preempt exit ordering.
        unsafe { self.scope.inner().unlock_shared() };
    }
}

/// Slot-level exclusive guard returned by [`ScopeCell::write`].
///
/// It intentionally does not dereference to `Scope`: active CPUs may retain a
/// shared identity for the stable inner object, so writers receive only the
/// item-level mutation capability authorized by the exclusive gate.
pub struct ScopeCellWriteGuard<'a> {
    inner: &'a ScopeInner,
    _preempt: Option<NoPreempt>,
}

impl ScopeCellWriteGuard<'_> {
    pub(crate) fn get_mut(&mut self, item: &'static Item) -> &mut ItemBox {
        // SAFETY: this guard owns the writer-preferred exclusive count. Slots
        // are UnsafeCell-backed so no `&mut Scope` aliases a published inner.
        unsafe { (&mut *self.inner.slot_ptr(item)).get_mut() }
    }
}

impl Drop for ScopeCellWriteGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: this guard owns the raw exclusive count. Its preemption guard
        // is dropped afterwards, preserving raw unlock -> preempt exit ordering.
        unsafe { self.inner.unlock_exclusive() };
    }
}

struct ItemSlot {
    value: ItemBox,
}

impl ItemSlot {
    fn new(item: &'static Item) -> Self {
        Self {
            value: ItemBox::new(item),
        }
    }

    fn get(&self) -> &ItemBox {
        &self.value
    }

    fn get_mut(&mut self) -> &mut ItemBox {
        &mut self.value
    }

    fn try_get(&self) -> Option<&ItemBox> {
        Some(&self.value)
    }
}

static GLOBAL_SCOPE: Once<Scope> = Once::new();
static GLOBAL_SCOPE_STATE: AtomicUsize = AtomicUsize::new(GlobalScopeState::Uninitialized as usize);

#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(usize)]
enum GlobalScopeState {
    Uninitialized,
    Ready,
}

struct GlobalInitialization {
    owner_context: usize,
    published: bool,
}

impl GlobalInitialization {
    fn begin(owner_context: usize) -> Self {
        Self {
            owner_context,
            published: false,
        }
    }

    fn publish(mut self, scope: Scope) {
        GLOBAL_SCOPE.call_once(|| scope);
        GLOBAL_SCOPE_STATE.store(GlobalScopeState::Ready as usize, Ordering::Release);
        self.published = true;
    }
}

impl Drop for GlobalInitialization {
    fn drop(&mut self) {
        if !self.published {
            let _ = GLOBAL_SCOPE_STATE.compare_exchange(
                self.owner_context,
                GlobalScopeState::Uninitialized as usize,
                Ordering::Release,
                Ordering::Relaxed,
            );
        }
    }
}

#[ax_percpu::def_percpu]
pub(crate) static ACTIVE_SCOPE_PTR: usize = 0;

/// Currently active scope.
pub struct ActiveScope;

impl ActiveScope {
    /// Sets the active scope pointer to the given scope.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the provided `scope` reference is valid for
    /// the duration in which it is set as the active scope, and that no data
    /// races or aliasing violations occur.
    pub unsafe fn set(scope: &Scope) {
        let _guard = NoPreempt::new();
        // SAFETY: the public contract supplies the scope lifetime and aliasing
        // invariants; NoPreempt keeps the guarded callback on this CPU.
        unsafe {
            ax_percpu::with_cpu_pin(|pin| Self::set_pinned(scope, pin))
                .expect("scope-local access requires an installed CPU area")
        };
    }

    /// Sets the active scope while borrowing an existing CPU pin.
    ///
    /// This variant performs no context transition and is therefore suitable
    /// for scheduler and hard-IRQ code that already owns a pin.
    ///
    /// # Safety
    ///
    /// The caller must keep `scope` alive for every current-CPU access until a
    /// later [`Self::set_global_pinned`] or pinned replacement, and must prevent
    /// concurrent mutable access to that scope's items.
    pub unsafe fn set_pinned(scope: &Scope, pin: &CpuPin<'_>) {
        ACTIVE_SCOPE_PTR.write_current(pin, scope.inner_ptr().expose_provenance());
    }

    /// Set the active scope to the global scope.
    ///
    /// # Safety
    ///
    /// The caller must own the current raw activation. In particular, this
    /// function must not clear a scheduler-managed [`ScopeCell`] activation;
    /// that activation must be released through [`ScopeCell::deactivate_pinned`].
    pub unsafe fn set_global() {
        let _guard = NoPreempt::new();
        // SAFETY: forwarded caller ownership applies to this pinned CPU.
        unsafe {
            ax_percpu::with_cpu_pin(|pin| Self::set_global_pinned(pin))
                .expect("scope-local access requires an installed CPU area")
        };
    }

    /// Sets the active scope to the global scope under an existing CPU pin.
    ///
    /// # Safety
    ///
    /// The caller must own the current raw activation and must not bypass a
    /// scheduler-managed [`ScopeCell`] activation.
    pub unsafe fn set_global_pinned(pin: &CpuPin<'_>) {
        ACTIVE_SCOPE_PTR.write_current(pin, 0);
    }

    /// Returns true if the active scope is the global scope.
    pub fn is_global() -> bool {
        let _guard = NoPreempt::new();
        // SAFETY: NoPreempt prevents migration for the complete callback.
        unsafe { ax_percpu::with_cpu_pin(Self::is_global_pinned) }
            .expect("scope-local access requires an installed CPU area")
    }

    /// Returns true if the active scope is global under an existing CPU pin.
    pub fn is_global_pinned(pin: &CpuPin<'_>) -> bool {
        ACTIVE_SCOPE_PTR.read_current(pin) == 0
    }

    pub(crate) fn with_item<'pin, R>(
        item: &'static Item,
        pin: &CpuPin<'pin>,
        operation: impl for<'access> FnOnce(&'access ItemBox) -> R,
    ) -> R {
        operation(Self::current_inner(pin).get_shared(item))
    }

    pub(crate) fn try_with_item<'pin, R>(
        item: &'static Item,
        pin: &CpuPin<'pin>,
        operation: impl for<'access> FnOnce(&'access ItemBox) -> R,
    ) -> Option<R> {
        Self::try_current_inner(pin)?
            .try_get_shared(item)
            .map(operation)
    }

    fn current_inner<'pin>(pin: &CpuPin<'pin>) -> &'pin ScopeInner {
        let ptr = ACTIVE_SCOPE_PTR.read_current(pin);
        let ptr = if ptr == 0 {
            NonNull::from_ref(
                GLOBAL_SCOPE
                    .get()
                    .expect("scope-local global scope must be initialized")
                    .inner(),
            )
        } else {
            NonNull::new(core::ptr::with_exposed_provenance_mut::<ScopeInner>(ptr))
                .expect("nonzero active scope address must reconstruct a pointer")
        };
        // SAFETY: set_pinned's contract keeps the selected scope live. A
        // scheduler-managed scope retains one shared lease for the activation;
        // the global scope is immutable after publication. The borrow is
        // shortened to the CPU pin lifetime.
        unsafe { ptr.as_ref() }
    }

    fn try_current_inner<'pin>(pin: &CpuPin<'pin>) -> Option<&'pin ScopeInner> {
        let ptr = ACTIVE_SCOPE_PTR.read_current(pin);
        let ptr = if ptr == 0 {
            NonNull::from_ref(GLOBAL_SCOPE.get()?.inner())
        } else {
            NonNull::new(core::ptr::with_exposed_provenance_mut::<ScopeInner>(ptr))?
        };
        // SAFETY: the same scope lifetime and pinning invariants as current_inner
        // apply. Unlike that path, GLOBAL_SCOPE.get never runs an
        // initializer and therefore remains valid in hard-IRQ context.
        Some(unsafe { ptr.as_ref() })
    }

    pub(crate) fn initialize_global() {
        let owner_context = current_context_identity();
        loop {
            match GLOBAL_SCOPE_STATE.load(Ordering::Acquire) {
                state if state == GlobalScopeState::Ready as usize => return,
                state if state == owner_context => {
                    panic!("scope-local global scope initialization is already in progress")
                }
                state if state == GlobalScopeState::Uninitialized as usize => {
                    if GLOBAL_SCOPE_STATE
                        .compare_exchange(
                            GlobalScopeState::Uninitialized as usize,
                            owner_context,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        let initialization = GlobalInitialization::begin(owner_context);
                        initialization.publish(Scope::new());
                        return;
                    }
                }
                _ => core::hint::spin_loop(),
            }
        }
    }

    fn current_scope_ptr_pinned(pin: &CpuPin<'_>) -> usize {
        ACTIVE_SCOPE_PTR.read_current(pin)
    }
}

fn current_context_identity() -> usize {
    let _guard = NoPreempt::new();
    // SAFETY: the guard keeps the current thread header stable while its opaque
    // identity is acquired. The header itself is pinned for the task lifetime,
    // so this identity remains valid if the task later migrates during an
    // initializer.
    let context = unsafe {
        ax_percpu::with_cpu_pin(|pin| pin.area().runtime_anchor().current_thread_raw())
            .expect("scope-local access requires an installed CPU area")
    };
    assert!(
        context > GlobalScopeState::Ready as usize,
        "scope-local initialization requires a valid current context"
    );
    context
}

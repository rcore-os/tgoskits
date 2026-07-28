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

/// Bounded raw gate for scheduler-owned scope leases.
///
/// Scheduler and IRQ-adjacent callers only attempt one state transition. They
/// never wait for a reader or writer while preemption is disabled.
struct ScopeGate {
    state: AtomicUsize,
}

impl ScopeGate {
    const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
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

    fn try_lock_exclusive(&self) -> bool {
        self.state
            .compare_exchange(0, SCOPE_GATE_WRITER, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn try_upgrade_active_shared_to_exclusive(&self) -> bool {
        self.state
            .compare_exchange(1, SCOPE_GATE_WRITER, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    unsafe fn downgrade_exclusive_to_active_shared(&self) {
        assert_eq!(
            self.state.load(Ordering::Relaxed),
            SCOPE_GATE_WRITER,
            "scope downgrade requires the exclusive lease"
        );
        self.state.store(1, Ordering::Release);
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
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{ScopeCell, ScopeGate};

    crate::scope_local! {
        static GATE_TEST_ITEM: usize = 0;
    }

    #[test]
    fn exclusive_attempt_is_bounded_by_live_readers() {
        let gate = ScopeGate::new();
        assert!(gate.try_lock_shared());
        assert!(!gate.try_lock_exclusive());
        assert!(gate.try_lock_shared());
        // SAFETY: the test acquired exactly two shared counts.
        unsafe {
            gate.unlock_shared();
            gate.unlock_shared();
        }
        assert!(gate.try_lock_exclusive());
        // SAFETY: the test acquired the exclusive count above.
        unsafe { gate.unlock_exclusive() };
        assert!(!gate.is_locked());
    }

    #[test]
    fn active_mutation_publishes_writer_before_releasing_its_lease() {
        let _retain_registry_entry = &GATE_TEST_ITEM;
        let cell = ScopeCell::new();
        assert!(cell.try_acquire_active_lease());
        let barged = AtomicBool::new(false);

        assert!(cell.try_withdraw_active_lease_for_writer(|| {
            let admitted = cell.scope.inner().gate.try_lock_shared();
            barged.store(admitted, Ordering::Relaxed);
            if admitted {
                // SAFETY: this callback acquired exactly one shared count.
                unsafe { cell.scope.inner().unlock_shared() };
            }
        }));
        // SAFETY: the production transition returned with the exclusive count.
        unsafe { cell.scope.inner().unlock_exclusive() };

        assert!(
            !barged.load(Ordering::Relaxed),
            "a new active lease entered after mutation began but before writer intent was visible"
        );
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

    fn try_lock_shared(&self) -> bool {
        self.gate.try_lock_shared()
    }

    fn try_lock_exclusive(&self) -> bool {
        self.gate.try_lock_exclusive()
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
        assert!(
            self.try_lock_shared(),
            "an exclusively borrowed scope cannot have a concurrent writer"
        );
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
/// no per-access lock. Every contended operation returns [`ScopeCellBusy`]
/// without waiting in a non-preemptible context.
pub struct ScopeCell {
    scope: Scope,
    active_cpus: AtomicUsize,
}

/// A bounded scope-cell lease could not be acquired immediately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopeCellBusy;

impl core::fmt::Display for ScopeCellBusy {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("scope cell is busy")
    }
}

impl core::error::Error for ScopeCellBusy {}

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

    /// Attempts to acquire an ordinary shared scope reference while preventing
    /// migration. It returns immediately when an exclusive lease is active.
    pub fn try_read(&self) -> Result<ScopeCellReadGuard<'_>, ScopeCellBusy> {
        let preempt = NoPreempt::new();
        if !self.scope.inner().gate.try_lock_shared() {
            return Err(ScopeCellBusy);
        }
        Ok(ScopeCellReadGuard {
            scope: &self.scope,
            _preempt: preempt,
        })
    }

    /// Attempts to acquire an ordinary exclusive scope reference while
    /// preventing migration. It returns immediately while any lease is live.
    pub fn try_write(&self) -> Result<ScopeCellWriteGuard<'_>, ScopeCellBusy> {
        let preempt = NoPreempt::new();
        let inner = self.scope.inner();
        if !inner.try_lock_exclusive() {
            return Err(ScopeCellBusy);
        }
        Ok(ScopeCellWriteGuard {
            inner,
            _preempt: Some(preempt),
            owns_exclusive: true,
        })
    }

    /// Attempts to install this scope for the pinned CPU and retain one shared
    /// lease.
    ///
    /// This operation does not enter a new IRQ or preemption context, so it is
    /// suitable for a scheduler switch-in hook that already owns its CPU-local
    /// baton.
    ///
    /// # Safety
    ///
    /// On success, the caller must keep this `ScopeCell` alive, retain the
    /// current CPU pin, and invoke
    /// [`deactivate_pinned`](Self::deactivate_pinned) exactly once before
    /// another scope is installed or the cell can be dropped. The scheduler
    /// must run both hooks while holding its switch baton.
    pub unsafe fn try_activate_pinned(&self, pin: &CpuPin<'_>) -> Result<(), ScopeCellBusy> {
        assert_eq!(
            ActiveScope::current_scope_ptr_pinned(pin),
            0,
            "scope activation requires the global scope to be current"
        );
        if !self.try_acquire_active_lease() {
            return Err(ScopeCellBusy);
        }
        // SAFETY: the caller contract keeps the cell live until deactivation,
        // and the retained shared lease excludes slot mutation.
        unsafe { ActiveScope::set_pinned(&self.scope, pin) };
        Ok(())
    }

    /// Clears this scope from the pinned CPU and retires its active identity.
    ///
    /// # Safety
    ///
    /// The current CPU must own exactly one activation previously established
    /// by [`try_activate_pinned`](Self::try_activate_pinned) for this cell.
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
    /// The active shared lease is atomically upgraded to the writer state and
    /// restored before this function returns. A remote reader or a second CPU
    /// activation returns [`ScopeCellBusy`] instead of making the caller spin.
    ///
    /// # Safety
    ///
    /// This cell must have exactly one activation, owned by the current CPU,
    /// and `pin` must remain valid for the complete call. The caller must
    /// prevent reentrant scope-local access while `operation` runs.
    pub unsafe fn try_with_active_mut_pinned<R>(
        &self,
        pin: &CpuPin<'_>,
        operation: impl for<'scope> FnOnce(&'scope mut ScopeCellWriteGuard<'_>) -> R,
    ) -> Result<R, ScopeCellBusy> {
        assert_eq!(
            ActiveScope::current_scope_ptr_pinned(pin),
            self.scope_ptr(),
            "active scope mutation does not match the current scope"
        );
        if !self.try_withdraw_active_lease_for_writer(|| {}) {
            return Err(ScopeCellBusy);
        }
        // SAFETY: the caller owns the sole activation verified above.
        unsafe { ActiveScope::set_global_pinned(pin) };
        let inner = self.scope.inner();
        let mut mutation = ActiveScopeMutation {
            cell: self,
            pin,
            writer: Some(ScopeCellWriteGuard {
                inner,
                _preempt: None,
                owns_exclusive: true,
            }),
        };
        let result = operation(mutation.writer());
        drop(mutation);
        Ok(result)
    }

    fn scope_ptr(&self) -> usize {
        self.scope.inner_ptr().expose_provenance()
    }

    fn try_acquire_active_lease(&self) -> bool {
        let inner = self.scope.inner();
        if !inner.try_lock_shared() {
            return false;
        }
        if self
            .active_cpus
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .is_err()
        {
            // SAFETY: this function acquired exactly one shared count above.
            unsafe { inner.unlock_shared() };
            return false;
        }
        true
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

    fn try_withdraw_active_lease_for_writer(&self, writer_pending: impl FnOnce()) -> bool {
        if self.active_cpus.load(Ordering::Acquire) != 1
            || !self
                .scope
                .inner()
                .gate
                .try_upgrade_active_shared_to_exclusive()
        {
            return false;
        }
        writer_pending();
        if self
            .active_cpus
            .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // SAFETY: this function atomically replaced the sole shared lease
            // with the exclusive lease, but the active-count invariant changed
            // before it could be retired. Restore the original shared state.
            unsafe {
                self.scope
                    .inner()
                    .gate
                    .downgrade_exclusive_to_active_shared()
            };
            return false;
        }
        true
    }

    fn restore_active_lease_from_writer(&self, pin: &CpuPin<'_>) {
        self.active_cpus
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .expect("active mutation restore found an unexpected activation");
        // SAFETY: the exclusive lease keeps the scope stable while the pinned
        // identity is restored. Downgrading publishes its shared lease before
        // the caller may re-enter scope-local access.
        unsafe {
            ActiveScope::set_pinned(&self.scope, pin);
            self.scope
                .inner()
                .gate
                .downgrade_exclusive_to_active_shared();
        }
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
        let mut writer = self
            .writer
            .take()
            .expect("active scope mutation writer must be present");
        self.cell.restore_active_lease_from_writer(self.pin);
        writer.owns_exclusive = false;
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

/// Shared ordinary-access guard returned by [`ScopeCell::try_read`].
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

/// Slot-level exclusive guard returned by [`ScopeCell::try_write`].
///
/// It intentionally does not dereference to `Scope`: active CPUs may retain a
/// shared identity for the stable inner object, so writers receive only the
/// item-level mutation capability authorized by the exclusive gate.
pub struct ScopeCellWriteGuard<'a> {
    inner: &'a ScopeInner,
    _preempt: Option<NoPreempt>,
    owns_exclusive: bool,
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
        if !self.owns_exclusive {
            return;
        }
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

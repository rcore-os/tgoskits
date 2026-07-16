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

use ax_kspin::PreemptGuard;
use ax_percpu::{BoundCpuPin, CpuPin};
use lock_api::{RawRwLock, RawRwLockUpgrade};
use spin::{Once, Spin, rwlock::RwLock};

use crate::{
    boxed::ItemBox,
    item::{Item, Registry},
};

/// A scope is a collection of items.
pub struct Scope {
    inner: Box<ScopeInner>,
}

struct ScopeInner {
    gate: RwLock<(), Spin>,
    slots: NonNull<UnsafeCell<ItemSlot>>,
}

// SAFETY: the public registration path admits only `Send + Sync + 'static`
// payloads, and ownership of every initialized slot moves with the Scope.
unsafe impl Send for Scope {}
// SAFETY: shared payload access is admitted only for `Sync` values and is
// serialized against mutation by `gate`.
unsafe impl Sync for Scope {}

impl Scope {
    /// Create a new namespace with all resources initialized as their default
    /// value.
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
            gate: RwLock::new(()),
            slots: ptr,
        }
    }

    fn lock_shared(&self) {
        RawRwLock::lock_shared(&self.gate);
    }

    fn lock_exclusive_writer_preferred(&self) {
        RawRwLockUpgrade::lock_upgradable(&self.gate);
        // SAFETY: this call owns the single upgradable count and consumes it
        // while waiting for bounded readers to drain. The upgradable bit keeps
        // new readers from barging ahead of the writer.
        unsafe { RawRwLockUpgrade::upgrade(&self.gate) };
    }

    unsafe fn unlock_shared(&self) {
        // SAFETY: forwarded to the caller, which must own one shared count.
        unsafe { RawRwLock::unlock_shared(&self.gate) };
    }

    unsafe fn unlock_exclusive(&self) {
        // SAFETY: forwarded to the caller, which must own the exclusive count.
        unsafe { RawRwLock::unlock_exclusive(&self.gate) };
    }

    pub(crate) fn with_item<R>(
        &self,
        item: &'static Item,
        operation: impl for<'access> FnOnce(&'access ItemBox) -> R,
    ) -> R {
        self.lock_shared();
        let lease = ScopeReadLease { inner: self };
        operation(lease.inner.get_shared(item))
    }

    pub(crate) fn try_with_item<R>(
        &self,
        item: &'static Item,
        operation: impl for<'access> FnOnce(&'access ItemBox) -> R,
    ) -> Option<R> {
        if !RawRwLock::try_lock_shared(&self.gate) {
            return None;
        }
        let lease = ScopeReadLease { inner: self };
        lease.inner.try_get_shared(item).map(operation)
    }

    pub(crate) fn read_item(&self, item: &'static Item) -> ScopeItemLease<'_> {
        self.lock_shared();
        ScopeItemLease { inner: self, item }
    }

    fn get_shared(&self, item: &'static Item) -> &ItemBox {
        let index = item.index();
        // SAFETY: every access to an ItemSlot is behind `gate`; a shared count
        // excludes the only mutable path.
        unsafe { (&*self.slots.add(index).as_ref().get()).get() }
    }

    fn try_get_shared(&self, item: &'static Item) -> Option<&ItemBox> {
        let index = item.index();
        // SAFETY: the caller owns one shared gate count.
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

struct ScopeReadLease<'scope> {
    inner: &'scope ScopeInner,
}

impl Drop for ScopeReadLease<'_> {
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

/// A scope whose scheduler binding is separate from bounded item access.
///
/// Scheduler hooks publish only a pinned pointer and never retain a lock or
/// context guard across a task lifetime. Each item operation takes a bounded
/// shared lease, while writers use an upgradable lease to publish writer intent
/// before waiting for existing readers.
pub struct ScopeCell {
    scope: Scope,
    active_cpus: AtomicUsize,
}

impl ScopeCell {
    /// Creates a managed scope with no active scheduler binding.
    pub fn new() -> Self {
        Self {
            scope: Scope::new(),
            active_cpus: AtomicUsize::new(0),
        }
    }

    /// Acquires an ordinary shared scope reference while preventing migration.
    pub fn read(&self) -> ScopeCellReadGuard<'_> {
        let preempt = PreemptGuard::new();
        self.scope.inner().lock_shared();
        ScopeCellReadGuard {
            scope: &self.scope,
            _preempt: preempt,
        }
    }

    /// Attempts to acquire an ordinary shared scope reference while preventing
    /// migration.
    pub fn try_read(&self) -> Option<ScopeCellReadGuard<'_>> {
        let preempt = PreemptGuard::new();
        if !RawRwLock::try_lock_shared(&self.scope.inner().gate) {
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
        let preempt = PreemptGuard::new();
        let inner = self.scope.inner();
        inner.lock_exclusive_writer_preferred();
        ScopeCellWriteGuard {
            inner,
            _preempt: Some(preempt),
        }
    }

    /// Installs this scope for the pinned CPU without retaining a lock count.
    ///
    /// This operation does not enter a new IRQ or preemption context, so it is
    /// suitable for a scheduler switch-in hook that already owns its CPU-local
    /// baton.
    ///
    /// # Safety
    ///
    /// The caller must keep this `ScopeCell` alive, retain the current CPU pin,
    /// and invoke [`deactivate_pinned`](Self::deactivate_pinned) exactly once
    /// before another scope is installed or the cell can be dropped. No
    /// scope-local item operation may span either scheduler hook.
    pub unsafe fn activate_pinned(&self, pin: &CpuPin) {
        assert_eq!(
            ActiveScope::current_scope_ptr_pinned(pin),
            0,
            "scope activation requires the global scope to be current"
        );
        self.active_cpus
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .expect("scope activation count overflow");
        // SAFETY: the caller contract keeps the cell live until deactivation;
        // bounded item accesses acquire the scope's own shared gate.
        unsafe { ActiveScope::set_pinned(&self.scope, pin) };
    }

    /// Clears this scope from the pinned CPU and retires its active identity.
    ///
    /// # Safety
    ///
    /// The current CPU must own exactly one activation previously established
    /// by [`activate_pinned`](Self::activate_pinned) for this cell.
    pub unsafe fn deactivate_pinned(&self, pin: &CpuPin) {
        assert_eq!(
            ActiveScope::current_scope_ptr_pinned(pin),
            self.scope_ptr(),
            "scope deactivation does not match the active scope"
        );
        // SAFETY: the caller owns this managed activation and therefore may
        // clear its raw per-CPU pointer.
        unsafe { ActiveScope::set_global_pinned(pin) };
        self.active_cpus
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .expect("scope deactivation without a matching activation");
    }

    /// Mutates the calling task's active scope without retaining a context-aware
    /// lock guard across scheduling boundaries.
    ///
    /// # Safety
    ///
    /// The current CPU must own exactly one activation for this cell and `pin`
    /// must remain valid for the complete call. The caller must prevent
    /// reentrant scope-local access while `operation` runs.
    pub unsafe fn with_active_mut_pinned<R>(
        &self,
        pin: &CpuPin,
        operation: impl for<'scope> FnOnce(&'scope mut ScopeCellWriteGuard<'_>) -> R,
    ) -> R {
        assert_eq!(
            ActiveScope::current_scope_ptr_pinned(pin),
            self.scope_ptr(),
            "active scope mutation does not match the current scope"
        );
        let inner = self.scope.inner();
        inner.lock_exclusive_writer_preferred();
        let mut guard = ScopeCellWriteGuard {
            inner,
            _preempt: None,
        };
        operation(&mut guard)
    }

    fn scope_ptr(&self) -> usize {
        self.scope.inner_ptr().expose_provenance()
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
            !RawRwLock::is_locked(&self.scope.inner().gate),
            "cannot drop a locked scope"
        );
    }
}

/// Shared ordinary-access guard returned by [`ScopeCell::read`].
pub struct ScopeCellReadGuard<'a> {
    scope: &'a Scope,
    _preempt: PreemptGuard,
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
    _preempt: Option<PreemptGuard>,
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
    item: &'static Item,
    value: Once<ItemBox>,
}

impl ItemSlot {
    fn new(item: &'static Item) -> Self {
        Self {
            item,
            value: Once::new(),
        }
    }

    fn get(&self) -> &ItemBox {
        self.value.call_once(|| ItemBox::new(self.item))
    }

    fn get_mut(&mut self) -> &mut ItemBox {
        if !self.value.is_completed() {
            let item = self.item;
            self.value.call_once(|| ItemBox::new(item));
        }
        self.value
            .get_mut()
            .expect("scope-local item must be initialized")
    }

    fn try_get(&self) -> Option<&ItemBox> {
        self.value.get()
    }
}

static GLOBAL_SCOPE: Once<Scope> = Once::new();

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
        let guard = PreemptGuard::new();
        // SAFETY: the public contract supplies the scope lifetime and aliasing
        // invariants; the guard supplies the current-CPU pin.
        unsafe { Self::set_pinned(scope, guard.cpu_pin()) };
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
    pub unsafe fn set_pinned(scope: &Scope, pin: &CpuPin) {
        let bound_pin = bound_current(pin);
        ACTIVE_SCOPE_PTR.write_current(&bound_pin, scope.inner_ptr().expose_provenance());
    }

    /// Set the active scope to the global scope.
    ///
    /// # Safety
    ///
    /// The caller must own the current raw activation. In particular, this
    /// function must not clear a scheduler-managed [`ScopeCell`] activation;
    /// that activation must be released through [`ScopeCell::deactivate_pinned`].
    pub unsafe fn set_global() {
        let guard = PreemptGuard::new();
        // SAFETY: forwarded caller ownership applies to this pinned CPU.
        unsafe { Self::set_global_pinned(guard.cpu_pin()) };
    }

    /// Sets the active scope to the global scope under an existing CPU pin.
    ///
    /// # Safety
    ///
    /// The caller must own the current raw activation and must not bypass a
    /// scheduler-managed [`ScopeCell`] activation.
    pub unsafe fn set_global_pinned(pin: &CpuPin) {
        let bound_pin = bound_current(pin);
        ACTIVE_SCOPE_PTR.write_current(&bound_pin, 0);
    }

    /// Returns true if the active scope is the global scope.
    pub fn is_global() -> bool {
        let guard = PreemptGuard::new();
        Self::is_global_pinned(guard.cpu_pin())
    }

    /// Returns true if the active scope is global under an existing CPU pin.
    pub fn is_global_pinned(pin: &CpuPin) -> bool {
        let bound_pin = bound_current(pin);
        ACTIVE_SCOPE_PTR.read_current(&bound_pin) == 0
    }

    pub(crate) fn with_item<R>(
        item: &'static Item,
        pin: &CpuPin,
        operation: impl for<'access> FnOnce(&'access ItemBox) -> R,
    ) -> R {
        Self::current_inner(pin).with_item(item, operation)
    }

    pub(crate) fn try_with_item<R>(
        item: &'static Item,
        pin: &CpuPin,
        operation: impl for<'access> FnOnce(&'access ItemBox) -> R,
    ) -> Option<R> {
        Self::try_current_inner(pin)?.try_with_item(item, operation)
    }

    fn current_inner(pin: &CpuPin) -> &ScopeInner {
        let bound_pin = bound_current(pin);
        let ptr = ACTIVE_SCOPE_PTR.read_current(&bound_pin);
        let ptr = if ptr == 0 {
            NonNull::from_ref(GLOBAL_SCOPE.call_once(Scope::new).inner())
        } else {
            NonNull::new(core::ptr::with_exposed_provenance_mut::<ScopeInner>(ptr))
                .expect("nonzero active scope address must reconstruct a pointer")
        };
        // SAFETY: set_pinned's contract keeps the selected scope live. Every
        // item access takes that scope's bounded gate before creating a payload
        // reference. The borrow is shortened to the CPU pin lifetime.
        unsafe { ptr.as_ref() }
    }

    fn try_current_inner(pin: &CpuPin) -> Option<&ScopeInner> {
        let bound_pin = bound_current(pin);
        let ptr = ACTIVE_SCOPE_PTR.read_current(&bound_pin);
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

    fn current_scope_ptr_pinned(pin: &CpuPin) -> usize {
        let bound_pin = bound_current(pin);
        ACTIVE_SCOPE_PTR.read_current(&bound_pin)
    }
}

fn bound_current(pin: &CpuPin) -> BoundCpuPin<'_> {
    ax_percpu::bound_current(pin).expect("scope-local access requires a bound CPU-local area")
}

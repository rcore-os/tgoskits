#![no_std]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{
    cell::UnsafeCell,
    ptr::{self, NonNull},
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

/// A platform IRQ number.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct IrqNumber(pub usize);

/// A logical CPU id.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct CpuId(pub usize);

/// A compact CPU mask for low-level IRQ affinity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuMask {
    bits: u128,
}

impl CpuMask {
    /// Creates an empty CPU mask.
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// Creates a CPU mask containing a single CPU.
    pub fn from_cpu(cpu: CpuId) -> Self {
        let mut mask = Self::empty();
        mask.insert(cpu);
        mask
    }

    /// Creates a CPU mask containing CPUs in the range `0..cpu_count`.
    pub fn first_n(cpu_count: usize) -> Self {
        let mut mask = Self::empty();
        let end = cpu_count.min(u128::BITS as usize);
        for cpu in 0..end {
            mask.insert(CpuId(cpu));
        }
        mask
    }

    /// Adds a CPU to this mask.
    pub fn insert(&mut self, cpu: CpuId) {
        if cpu.0 < u128::BITS as usize {
            self.bits |= 1u128 << cpu.0;
        }
    }

    /// Removes a CPU from this mask.
    pub fn remove(&mut self, cpu: CpuId) {
        if cpu.0 < u128::BITS as usize {
            self.bits &= !(1u128 << cpu.0);
        }
    }

    /// Returns whether the CPU is present in this mask.
    pub const fn contains(self, cpu: CpuId) -> bool {
        cpu.0 < u128::BITS as usize && (self.bits & (1u128 << cpu.0)) != 0
    }

    /// Returns whether no CPU is present in this mask.
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// Iterates over the CPUs in this mask.
    pub fn iter(self) -> CpuMaskIter {
        CpuMaskIter { bits: self.bits }
    }
}

/// Iterator over [`CpuMask`].
pub struct CpuMaskIter {
    bits: u128,
}

impl Iterator for CpuMaskIter {
    type Item = CpuId;

    fn next(&mut self) -> Option<Self::Item> {
        if self.bits == 0 {
            return None;
        }
        let cpu = self.bits.trailing_zeros() as usize;
        self.bits &= !(1u128 << cpu);
        Some(CpuId(cpu))
    }
}

/// IRQ registration scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqScope {
    /// The action is visible on every CPU.
    Global,
    /// The action is CPU-local and only visible to matching CPUs.
    PerCpu {
        /// Target CPUs.
        cpus: CpuMask,
    },
}

/// Whether an IRQ line is exclusive or shared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShareMode {
    /// No other action can share the IRQ.
    Exclusive,
    /// Multiple actions can share the IRQ if their trigger mode is compatible.
    Shared,
}

/// IRQ trigger mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriggerMode {
    /// The framework should not constrain the platform trigger mode.
    Unspecified,
    /// Edge triggered IRQ.
    Edge,
    /// Active-high level triggered IRQ.
    LevelHigh,
    /// Active-low level triggered IRQ.
    LevelLow,
}

/// Whether an IRQ action should be enabled after registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoEnable {
    /// Register the action but leave it disabled.
    No,
    /// Enable the action after registration.
    Yes,
}

/// Return value from a raw IRQ handler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqReturn {
    /// This action did not handle the IRQ.
    Unhandled,
    /// This action handled the IRQ.
    Handled,
    /// This action handled the IRQ and asks the OS adapter to wake deferred work.
    Wake,
}

/// Aggregated dispatch result.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IrqOutcome {
    /// At least one action handled the IRQ.
    pub handled: bool,
    /// At least one action requested a wakeup.
    pub wake: bool,
    /// Number of handlers called by this dispatch.
    pub called: usize,
}

/// IRQ status snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqStatus {
    /// Whether this action is enabled in the framework.
    pub action_enabled: bool,
    /// Whether the platform line is enabled.
    pub line_enabled: bool,
    /// Whether the platform reports the IRQ pending.
    pub pending: bool,
    /// Whether the platform reports the IRQ in service.
    pub in_service: bool,
    /// Number of in-flight dispatches for this descriptor.
    pub in_flight: usize,
}

/// IRQ framework errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqError {
    /// Invalid IRQ number.
    InvalidIrq,
    /// Invalid CPU id.
    InvalidCpu,
    /// The target CPU is offline.
    CpuOffline,
    /// IRQ line/action sharing rules reject the operation.
    Busy,
    /// Allocation failed.
    NoMemory,
    /// The requested descriptor or action does not exist.
    NotFound,
    /// This operation is not legal from IRQ context.
    InIrqContext,
    /// The platform adapter does not support this operation.
    Unsupported,
    /// The platform controller reported an error.
    Controller,
}

/// Context passed to IRQ handlers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqContext {
    /// IRQ number being dispatched.
    pub irq: IrqNumber,
    /// CPU handling the IRQ.
    pub cpu: CpuId,
}

/// Raw IRQ handler ABI.
pub type RawIrqHandler = unsafe fn(ctx: IrqContext, data: NonNull<()>) -> IrqReturn;

/// External capabilities supplied by the OS/platform adapter.
pub trait IrqOps {
    /// Saved local IRQ state.
    type LocalIrqState: Copy;

    /// Returns the current CPU.
    fn current_cpu(&self) -> CpuId;

    /// Returns whether the CPU is online.
    fn cpu_online(&self, cpu: CpuId) -> bool;

    /// Returns whether the current execution context is an IRQ context.
    fn in_irq_context(&self) -> bool;

    /// Saves and disables local IRQs for metadata lock acquisition.
    fn local_irq_save(&self) -> Self::LocalIrqState;

    /// Restores local IRQ state saved by [`IrqOps::local_irq_save`].
    fn local_irq_restore(&self, state: Self::LocalIrqState);

    /// Runs a thunk synchronously on the target CPU.
    fn run_on_cpu_sync(
        &self,
        cpu: CpuId,
        f: unsafe fn(*mut ()),
        arg: *mut (),
    ) -> Result<(), IrqError>;

    /// Enables or disables an IRQ line.
    fn set_enabled(
        &self,
        irq: IrqNumber,
        cpu: Option<CpuId>,
        enabled: bool,
    ) -> Result<(), IrqError>;

    /// Returns whether the IRQ line is enabled.
    fn is_enabled(&self, irq: IrqNumber, cpu: Option<CpuId>) -> Result<bool, IrqError>;

    /// Returns whether the IRQ line is pending.
    fn is_pending(&self, irq: IrqNumber, cpu: Option<CpuId>) -> Result<bool, IrqError>;

    /// Returns whether the IRQ line is in service.
    fn is_in_service(&self, irq: IrqNumber, cpu: Option<CpuId>) -> Result<bool, IrqError>;

    /// Relaxes a spin wait.
    fn relax(&self);
}

/// Request parameters for an IRQ action.
#[derive(Clone, Copy, Debug)]
pub struct IrqRequest {
    handler: RawIrqHandler,
    data: NonNull<()>,
    scope: IrqScope,
    share_mode: ShareMode,
    trigger: TriggerMode,
    auto_enable: AutoEnable,
}

impl IrqRequest {
    /// Creates a new exclusive, global, auto-enabled IRQ request.
    pub const fn new(handler: RawIrqHandler, data: NonNull<()>) -> Self {
        Self {
            handler,
            data,
            scope: IrqScope::Global,
            share_mode: ShareMode::Exclusive,
            trigger: TriggerMode::Unspecified,
            auto_enable: AutoEnable::Yes,
        }
    }

    /// Sets the IRQ scope.
    pub const fn scope(mut self, scope: IrqScope) -> Self {
        self.scope = scope;
        self
    }

    /// Sets the sharing mode.
    pub const fn share_mode(mut self, share_mode: ShareMode) -> Self {
        self.share_mode = share_mode;
        self
    }

    /// Sets the trigger mode.
    pub const fn trigger(mut self, trigger: TriggerMode) -> Self {
        self.trigger = trigger;
        self
    }

    /// Sets whether the action should be enabled after request.
    pub const fn auto_enable(mut self, auto_enable: AutoEnable) -> Self {
        self.auto_enable = auto_enable;
        self
    }
}

/// Token returned from request and used for later lifecycle operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqHandle {
    irq: IrqNumber,
    id: u64,
}

impl IrqHandle {
    /// Returns the IRQ number associated with this handle.
    pub const fn irq(self) -> IrqNumber {
        self.irq
    }

    /// Returns the framework-local action id.
    pub const fn id(self) -> u64 {
        self.id
    }
}

struct Action {
    id: u64,
    handler: RawIrqHandler,
    data: NonNull<()>,
    scope: IrqScope,
    enabled: AtomicBool,
    detached: AtomicBool,
    pending_enable: UnsafeCell<CpuMask>,
    next: *mut Action,
}

// Raw handler context pointers are owned by the OS adapter. The framework only
// stores and passes them back to the registered handler.
unsafe impl Send for Action {}
unsafe impl Sync for Action {}

impl Action {
    fn pending_enable_contains(&self, cpu: CpuId) -> bool {
        unsafe { (&*self.pending_enable.get()).contains(cpu) }
    }

    fn insert_pending_enable(&self, cpu: CpuId) {
        unsafe { (&mut *self.pending_enable.get()).insert(cpu) };
    }

    fn remove_pending_enable(&self, cpu: CpuId) {
        unsafe { (&mut *self.pending_enable.get()).remove(cpu) };
    }

    fn clear_pending_enable_all(&self) {
        unsafe { *self.pending_enable.get() = CpuMask::empty() };
    }
}

struct Descriptor {
    irq: IrqNumber,
    share_mode: ShareMode,
    trigger: TriggerMode,
    in_flight: AtomicUsize,
    line_desired: bool,
    line_applied: bool,
    percpu_line_desired: CpuMask,
    percpu_line_applied: CpuMask,
    head: *mut Action,
}

impl Descriptor {
    fn new(irq: IrqNumber, request: &IrqRequest) -> Self {
        Self {
            irq,
            share_mode: request.share_mode,
            trigger: request.trigger,
            in_flight: AtomicUsize::new(0),
            line_desired: false,
            line_applied: false,
            percpu_line_desired: CpuMask::empty(),
            percpu_line_applied: CpuMask::empty(),
            head: ptr::null_mut(),
        }
    }

    fn compatible_with(&mut self, request: &IrqRequest) -> Result<(), IrqError> {
        let has_active_actions = self.actions().any(|action| {
            let action = unsafe { &*action };
            !action.detached.load(Ordering::Acquire)
        });

        if !has_active_actions {
            self.share_mode = request.share_mode;
            self.trigger = request.trigger;
            return Ok(());
        }

        if self.share_mode != ShareMode::Shared || request.share_mode != ShareMode::Shared {
            return Err(IrqError::Busy);
        }

        match (self.trigger, request.trigger) {
            (TriggerMode::Unspecified, trigger) => self.trigger = trigger,
            (current, TriggerMode::Unspecified) => {
                let _ = current;
            }
            (current, requested) if current == requested => {}
            _ => return Err(IrqError::Busy),
        }

        Ok(())
    }

    fn actions(&self) -> ActionIter {
        ActionIter { next: self.head }
    }

    fn line_desired(&self, cpu: Option<CpuId>) -> bool {
        match cpu {
            Some(cpu) => self.percpu_line_desired.contains(cpu),
            None => self.line_desired,
        }
    }

    fn line_applied(&self, cpu: Option<CpuId>) -> bool {
        match cpu {
            Some(cpu) => self.percpu_line_applied.contains(cpu),
            None => self.line_applied,
        }
    }

    fn set_line_desired(&mut self, cpu: Option<CpuId>, enabled: bool) {
        match cpu {
            Some(cpu) => {
                if enabled {
                    self.percpu_line_desired.insert(cpu);
                } else {
                    self.percpu_line_desired.remove(cpu);
                }
            }
            None => self.line_desired = enabled,
        }
    }

    fn set_line_applied(&mut self, cpu: Option<CpuId>, enabled: bool) {
        match cpu {
            Some(cpu) => {
                if enabled {
                    self.percpu_line_applied.insert(cpu);
                } else {
                    self.percpu_line_applied.remove(cpu);
                }
            }
            None => self.line_applied = enabled,
        }
    }

    fn recompute_line_desired(&mut self, cpu: Option<CpuId>) {
        let desired = self.actions().any(|action| {
            let action = unsafe { &*action };
            !action.detached.load(Ordering::Acquire)
                && action.enabled.load(Ordering::Acquire)
                && cpu.is_none_or(|cpu| action_matches_cpu(action.scope, cpu))
        });
        self.set_line_desired(cpu, desired);
    }
}

struct ActionIter {
    next: *mut Action,
}

impl Iterator for ActionIter {
    type Item = *mut Action;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next.is_null() {
            return None;
        }
        let current = self.next;
        self.next = unsafe { (*current).next };
        Some(current)
    }
}

/// Dynamic IRQ registry.
pub struct Registry<O: IrqOps> {
    ops: O,
    lock: MetadataLock,
    next_id: AtomicU64,
    state: UnsafeCell<RegistryState>,
}

unsafe impl<O: IrqOps + Send> Send for Registry<O> {}
unsafe impl<O: IrqOps + Send> Sync for Registry<O> {}

struct RegistryState {
    descriptors: Vec<Descriptor>,
}

impl RegistryState {
    fn new() -> Self {
        Self {
            descriptors: Vec::new(),
        }
    }
}

struct MetadataLock {
    locked: AtomicBool,
}

impl MetadataLock {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    fn lock<O: IrqOps>(&self, ops: &O) -> O::LocalIrqState {
        let state = ops.local_irq_save();
        while self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            ops.relax();
        }
        state
    }

    fn unlock<O: IrqOps>(&self, ops: &O, state: O::LocalIrqState) {
        self.locked.store(false, Ordering::Release);
        ops.local_irq_restore(state);
    }
}

impl<O: IrqOps> Registry<O> {
    /// Creates an empty registry.
    pub fn new(ops: O) -> Self {
        Self {
            ops,
            lock: MetadataLock::new(),
            next_id: AtomicU64::new(1),
            state: UnsafeCell::new(RegistryState::new()),
        }
    }

    /// Registers an IRQ action.
    pub fn request(&self, irq: IrqNumber, request: IrqRequest) -> Result<IrqHandle, IrqError> {
        self.validate_request(&request)?;

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let action = Box::new(Action {
            id,
            handler: request.handler,
            data: request.data,
            scope: request.scope,
            enabled: AtomicBool::new(false),
            detached: AtomicBool::new(false),
            pending_enable: UnsafeCell::new(CpuMask::empty()),
            next: ptr::null_mut(),
        });

        let action = Box::into_raw(action);
        let irq_state = self.lock.lock(&self.ops);
        let result = self.insert_action_locked(irq, &request, action);
        self.lock.unlock(&self.ops, irq_state);

        if let Err(err) = result {
            unsafe {
                drop(Box::from_raw(action));
            }
            return Err(err);
        }

        let handle = IrqHandle { irq, id };
        if request.auto_enable == AutoEnable::Yes
            && let Err(err) = self.enable(handle)
        {
            self.drop_detached_action(handle);
            return Err(err);
        }
        Ok(handle)
    }

    /// Frees an IRQ action.
    pub fn free(&self, handle: IrqHandle) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        let (action, scope) = self.detach_action(handle)?;
        let mut result = self.apply_scope_line_state(handle.irq, scope);
        if let Err(err) = self.wait_and_remove_action(handle.irq, action)
            && result.is_ok()
        {
            result = Err(err);
        }
        unsafe {
            drop(Box::from_raw(action));
        }
        result
    }

    /// Enables an IRQ action and its backing line.
    pub fn enable(&self, handle: IrqHandle) -> Result<(), IrqError> {
        let scope = self.set_action_enabled(handle, true)?;

        if let Err(err) = self.apply_enabled(handle, scope, true) {
            let _ = self.disable(handle);
            return Err(err);
        }
        Ok(())
    }

    /// Disables an IRQ action and its backing line.
    pub fn disable(&self, handle: IrqHandle) -> Result<(), IrqError> {
        let scope = self.set_action_enabled(handle, false)?;
        self.apply_enabled(handle, scope, false)
    }

    fn set_action_enabled(&self, handle: IrqHandle, enabled: bool) -> Result<IrqScope, IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == handle.irq)
                .ok_or(IrqError::NotFound)?;
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.load(Ordering::Acquire) {
                    return Err(IrqError::NotFound);
                }
                (*action).enabled.store(enabled, Ordering::Release);
                (*action).clear_pending_enable_all();
                let scope = (*action).scope;
                recompute_scope_line_desired(descriptor, scope);
                Ok(scope)
            }
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    /// Returns a status snapshot for an IRQ action.
    pub fn status(&self, handle: IrqHandle) -> Result<IrqStatus, IrqError> {
        let (scope, action_enabled, in_flight) = self.with_action(handle, |action| {
            let in_flight = self
                .descriptor(handle.irq)
                .map(|desc| desc.in_flight.load(Ordering::Acquire))
                .unwrap_or(0);
            (
                action.scope,
                action.enabled.load(Ordering::Acquire),
                in_flight,
            )
        })?;
        let cpu = status_cpu(scope, self.ops.current_cpu());
        let line_enabled = match self.ops.is_enabled(handle.irq, cpu) {
            Ok(enabled) => enabled,
            Err(IrqError::Unsupported) => self.framework_line_enabled(handle.irq, cpu)?,
            Err(err) => return Err(err),
        };
        let pending = match self.ops.is_pending(handle.irq, cpu) {
            Ok(pending) => pending,
            Err(IrqError::Unsupported) => false,
            Err(err) => return Err(err),
        };
        let in_service = match self.ops.is_in_service(handle.irq, cpu) {
            Ok(in_service) => in_service,
            Err(IrqError::Unsupported) => false,
            Err(err) => return Err(err),
        };
        Ok(IrqStatus {
            action_enabled,
            line_enabled,
            pending,
            in_service,
            in_flight,
        })
    }

    /// Dispatches an IRQ on the given CPU.
    pub fn dispatch(&self, irq: IrqNumber, cpu: CpuId) -> IrqOutcome {
        let Some(head) = self.begin_dispatch(irq) else {
            return IrqOutcome::default();
        };
        let _guard = DispatchGuard {
            registry: self,
            irq,
        };

        let mut outcome = IrqOutcome::default();
        let ctx = IrqContext { irq, cpu };
        let mut next = head;
        while !next.is_null() {
            let action = unsafe { &*next };
            next = action.next;
            if action.detached.load(Ordering::Acquire)
                || !action.enabled.load(Ordering::Acquire)
                || !action_matches_cpu(action.scope, cpu)
            {
                continue;
            }

            outcome.called += 1;
            match unsafe { (action.handler)(ctx, action.data) } {
                IrqReturn::Unhandled => {}
                IrqReturn::Handled => outcome.handled = true,
                IrqReturn::Wake => {
                    outcome.handled = true;
                    outcome.wake = true;
                }
            }
        }

        outcome
    }

    /// Marks a CPU online and applies pending per-CPU enables for that CPU.
    pub fn cpu_online(&self, cpu: CpuId) -> Result<(), IrqError> {
        if !self.ops.cpu_online(cpu) {
            return Err(IrqError::CpuOffline);
        }
        let pending = self.pending_enables_for_cpu(cpu);
        for irq in pending {
            self.apply_line_state(irq, Some(cpu))?;
            self.clear_pending_enable_for_cpu(irq, cpu);
        }
        Ok(())
    }

    /// Marks a CPU offline from the framework's perspective.
    pub fn cpu_offline(&self, cpu: CpuId) -> Result<(), IrqError> {
        if self.ops.cpu_online(cpu) {
            return Err(IrqError::Unsupported);
        }
        Ok(())
    }

    fn validate_request(&self, request: &IrqRequest) -> Result<(), IrqError> {
        if let IrqScope::PerCpu { cpus } = request.scope
            && cpus.is_empty()
        {
            return Err(IrqError::InvalidCpu);
        }
        Ok(())
    }

    fn insert_action_locked(
        &self,
        irq: IrqNumber,
        request: &IrqRequest,
        action: *mut Action,
    ) -> Result<(), IrqError> {
        let state = unsafe { &mut *self.state.get() };
        let descriptor = match state
            .descriptors
            .iter_mut()
            .find(|descriptor| descriptor.irq == irq)
        {
            Some(descriptor) => descriptor,
            None => {
                state.descriptors.push(Descriptor::new(irq, request));
                state.descriptors.last_mut().ok_or(IrqError::NoMemory)?
            }
        };
        descriptor.compatible_with(request)?;
        unsafe {
            (*action).next = descriptor.head;
        }
        descriptor.head = action;
        Ok(())
    }

    fn detach_action(&self, handle: IrqHandle) -> Result<(*mut Action, IrqScope), IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == handle.irq)
                .ok_or(IrqError::NotFound)?;
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.swap(true, Ordering::AcqRel) {
                    return Err(IrqError::NotFound);
                }
                (*action).enabled.store(false, Ordering::Release);
                (*action).clear_pending_enable_all();
                let scope = (*action).scope;
                recompute_scope_line_desired(descriptor, scope);
                Ok((action, scope))
            }
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn wait_and_remove_action(&self, irq: IrqNumber, action: *mut Action) -> Result<(), IrqError> {
        loop {
            match self.try_remove_action(irq, action) {
                Err(IrqError::Busy) => self.ops.relax(),
                result => return result,
            }
        }
    }

    fn try_remove_action(&self, irq: IrqNumber, action: *mut Action) -> Result<(), IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == irq)
                .ok_or(IrqError::NotFound)?;
            if descriptor.in_flight.load(Ordering::Acquire) != 0 {
                return Err(IrqError::Busy);
            }
            let mut link = &mut descriptor.head as *mut *mut Action;
            while unsafe { !(*link).is_null() } {
                let current = unsafe { *link };
                if current == action {
                    unsafe {
                        *link = (*current).next;
                        (*current).next = ptr::null_mut();
                    }
                    return Ok(());
                }
                link = unsafe { &mut (*current).next as *mut *mut Action };
            }
            Err(IrqError::NotFound)
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn drop_detached_action(&self, handle: IrqHandle) {
        if let Ok((action, _scope)) = self.detach_action(handle)
            && self.wait_and_remove_action(handle.irq, action).is_ok()
        {
            unsafe {
                drop(Box::from_raw(action));
            }
        }
    }

    fn with_action<T>(
        &self,
        handle: IrqHandle,
        f: impl FnOnce(&Action) -> T,
    ) -> Result<T, IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let action = self.find_action(handle).ok_or(IrqError::NotFound)?;
            Ok(f(action))
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn apply_enabled(
        &self,
        handle: IrqHandle,
        scope: IrqScope,
        enabled: bool,
    ) -> Result<(), IrqError> {
        match scope {
            IrqScope::Global => self.apply_line_state(handle.irq, None),
            IrqScope::PerCpu { cpus } => {
                for cpu in cpus.iter() {
                    self.apply_percpu_enabled(handle, cpu, enabled)?;
                }
                Ok(())
            }
        }
    }

    fn apply_percpu_enabled(
        &self,
        handle: IrqHandle,
        cpu: CpuId,
        enabled: bool,
    ) -> Result<(), IrqError> {
        if self.ops.cpu_online(cpu) {
            self.apply_line_state(handle.irq, Some(cpu))?;
        } else if enabled {
            self.with_action(handle, |action| {
                action.insert_pending_enable(cpu);
            })?;
        } else {
            self.with_action(handle, |action| {
                action.remove_pending_enable(cpu);
            })?;
        }
        Ok(())
    }

    fn apply_scope_line_state(&self, irq: IrqNumber, scope: IrqScope) -> Result<(), IrqError> {
        match scope {
            IrqScope::Global => self.apply_line_state(irq, None),
            IrqScope::PerCpu { cpus } => {
                for cpu in cpus.iter() {
                    self.apply_line_state(irq, Some(cpu))?;
                }
                Ok(())
            }
        }
    }

    fn apply_line_state(&self, irq: IrqNumber, cpu: Option<CpuId>) -> Result<(), IrqError> {
        loop {
            if let Some(cpu) = cpu
                && !self.ops.cpu_online(cpu)
            {
                return Ok(());
            }

            let Some((desired, applied)) = self.line_state(irq, cpu) else {
                return Err(IrqError::NotFound);
            };
            if desired == applied {
                return Ok(());
            }

            self.set_controller_enabled(irq, cpu, desired)?;
            self.set_line_applied(irq, cpu, desired)?;
        }
    }

    fn set_controller_enabled(
        &self,
        irq: IrqNumber,
        cpu: Option<CpuId>,
        enabled: bool,
    ) -> Result<(), IrqError> {
        match cpu {
            None => self.ops.set_enabled(irq, None, enabled),
            Some(cpu) if cpu == self.ops.current_cpu() => {
                self.ops.set_enabled(irq, Some(cpu), enabled)
            }
            Some(cpu) => {
                let mut request = RemoteEnable {
                    registry: self as *const Self as *mut (),
                    irq,
                    cpu,
                    enabled,
                    result: Ok(()),
                };
                self.ops.run_on_cpu_sync(
                    cpu,
                    remote_enable_thunk::<O>,
                    (&mut request as *mut RemoteEnable).cast(),
                )?;
                request.result
            }
        }
    }

    fn begin_dispatch(&self, irq: IrqNumber) -> Option<*mut Action> {
        let irq_state = self.lock.lock(&self.ops);
        let result = {
            let state = unsafe { &mut *self.state.get() };
            state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == irq)
                .and_then(|descriptor| {
                    if descriptor.head.is_null() {
                        None
                    } else {
                        descriptor.in_flight.fetch_add(1, Ordering::AcqRel);
                        Some(descriptor.head)
                    }
                })
        };
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn end_dispatch(&self, irq: IrqNumber) {
        let irq_state = self.lock.lock(&self.ops);
        let state = unsafe { &mut *self.state.get() };
        if let Some(descriptor) = state
            .descriptors
            .iter_mut()
            .find(|descriptor| descriptor.irq == irq)
        {
            descriptor.in_flight.fetch_sub(1, Ordering::AcqRel);
        }
        self.lock.unlock(&self.ops, irq_state);
    }

    fn pending_enables_for_cpu(&self, cpu: CpuId) -> Vec<IrqNumber> {
        let irq_state = self.lock.lock(&self.ops);
        let mut pending = Vec::new();
        for descriptor in &self.state_ref().descriptors {
            if descriptor.actions().any(|action| {
                let action = unsafe { &*action };
                !action.detached.load(Ordering::Acquire)
                    && action.pending_enable_contains(cpu)
                    && action_matches_cpu(action.scope, cpu)
            }) {
                pending.push(descriptor.irq);
            }
        }
        self.lock.unlock(&self.ops, irq_state);
        pending
    }

    fn clear_pending_enable_for_cpu(&self, irq: IrqNumber, cpu: CpuId) {
        let irq_state = self.lock.lock(&self.ops);
        if let Some(descriptor) = self.descriptor(irq) {
            for action in descriptor.actions() {
                let action = unsafe { &*action };
                if action_matches_cpu(action.scope, cpu) {
                    action.remove_pending_enable(cpu);
                }
            }
        }
        self.lock.unlock(&self.ops, irq_state);
    }

    fn line_state(&self, irq: IrqNumber, cpu: Option<CpuId>) -> Option<(bool, bool)> {
        let irq_state = self.lock.lock(&self.ops);
        let result = self
            .descriptor(irq)
            .map(|descriptor| (descriptor.line_desired(cpu), descriptor.line_applied(cpu)));
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn set_line_applied(
        &self,
        irq: IrqNumber,
        cpu: Option<CpuId>,
        enabled: bool,
    ) -> Result<(), IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == irq)
                .ok_or(IrqError::NotFound)?;
            descriptor.set_line_applied(cpu, enabled);
            Ok(())
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn framework_line_enabled(&self, irq: IrqNumber, cpu: Option<CpuId>) -> Result<bool, IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let descriptor = self.descriptor(irq).ok_or(IrqError::NotFound)?;
            Ok(descriptor.line_applied(cpu))
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn find_action(&self, handle: IrqHandle) -> Option<&Action> {
        self.descriptor(handle.irq)?
            .actions()
            .map(|action| unsafe { &*action })
            .find(|action| action.id == handle.id && !action.detached.load(Ordering::Acquire))
    }

    fn descriptor(&self, irq: IrqNumber) -> Option<&Descriptor> {
        self.state_ref()
            .descriptors
            .iter()
            .find(|descriptor| descriptor.irq == irq)
    }

    fn state_ref(&self) -> &RegistryState {
        unsafe { &*self.state.get() }
    }
}

struct DispatchGuard<'a, O: IrqOps> {
    registry: &'a Registry<O>,
    irq: IrqNumber,
}

impl<O: IrqOps> Drop for DispatchGuard<'_, O> {
    fn drop(&mut self) {
        self.registry.end_dispatch(self.irq);
    }
}

struct RemoteEnable {
    registry: *mut (),
    irq: IrqNumber,
    cpu: CpuId,
    enabled: bool,
    result: Result<(), IrqError>,
}

unsafe fn remote_enable_thunk<O: IrqOps>(arg: *mut ()) {
    let request = unsafe { &mut *arg.cast::<RemoteEnable>() };
    let registry = unsafe { &*(request.registry as *const Registry<O>) };
    request.result = registry
        .ops
        .set_enabled(request.irq, Some(request.cpu), request.enabled);
}

fn action_matches_cpu(scope: IrqScope, cpu: CpuId) -> bool {
    match scope {
        IrqScope::Global => true,
        IrqScope::PerCpu { cpus } => cpus.contains(cpu),
    }
}

fn recompute_scope_line_desired(descriptor: &mut Descriptor, scope: IrqScope) {
    match scope {
        IrqScope::Global => descriptor.recompute_line_desired(None),
        IrqScope::PerCpu { cpus } => {
            for cpu in cpus.iter() {
                descriptor.recompute_line_desired(Some(cpu));
            }
        }
    }
}

fn status_cpu(scope: IrqScope, current: CpuId) -> Option<CpuId> {
    match scope {
        IrqScope::Global => None,
        IrqScope::PerCpu { .. } => Some(current),
    }
}

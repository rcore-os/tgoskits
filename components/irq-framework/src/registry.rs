use alloc::{boxed::Box, vec::Vec};
use core::{
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    CpuId, IrqAffinity, IrqContext, IrqError, IrqExecution, IrqHandle, IrqNumber, IrqOps,
    IrqOutcome, IrqRequest, IrqReturn, IrqScope, IrqStatus,
    action::Action,
    descriptor::{Descriptor, action_matches_cpu, recompute_scope_line_desired},
    lock::MetadataLock,
};

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

        let snapshot = self.snapshot_and_disable_scope_line(irq, request.scope)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let action = Box::new(Action::new(id, &request));
        let action = Box::into_raw(action);
        let irq_state = self.lock.lock(&self.ops);
        let result = self.insert_action_locked(irq, &request, action);
        self.lock.unlock(&self.ops, irq_state);

        if let Err(err) = result {
            unsafe {
                drop(Box::from_raw(action));
            }
            let _ = self.restore_scope_line_snapshot(irq, request.scope, &snapshot);
            return Err(err);
        }

        let handle = IrqHandle { irq, id };
        if let Err(err) = self.apply_affinity(irq, request.affinity) {
            self.drop_detached_action(handle);
            let _ = self.restore_scope_line_snapshot(irq, request.scope, &snapshot);
            return Err(err);
        }
        let restore_result = self.restore_scope_line_snapshot(irq, request.scope, &snapshot);
        if let Err(err) = restore_result {
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

    /// Waits until no handler is in flight for this IRQ descriptor.
    pub fn synchronize(&self, handle: IrqHandle) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        loop {
            let in_flight = self.with_action(handle, |_| {
                self.descriptor(handle.irq)
                    .map(|desc| desc.in_flight.load(Ordering::Acquire))
                    .unwrap_or(0)
            })?;
            if in_flight == 0 {
                return Ok(());
            }
            self.ops.relax();
        }
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
        let action_running =
            self.with_action(handle, |action| action.running.load(Ordering::Acquire))?;
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
            action_running,
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

            let Some(_guard) = ActionRunGuard::enter(action) else {
                continue;
            };

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
        if let IrqAffinity::Fixed(cpu) = request.affinity
            && !self.ops.cpu_online(cpu)
        {
            return Err(IrqError::CpuOffline);
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
        recompute_scope_line_desired(descriptor, request.scope);
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

    fn apply_affinity(&self, irq: IrqNumber, affinity: IrqAffinity) -> Result<(), IrqError> {
        match affinity {
            IrqAffinity::Any => Ok(()),
            IrqAffinity::Fixed(cpu) if self.ops.cpu_online(cpu) => {
                self.ops.set_affinity(irq, affinity)
            }
            IrqAffinity::Fixed(_) => Err(IrqError::CpuOffline),
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

    fn snapshot_and_disable_scope_line(
        &self,
        irq: IrqNumber,
        scope: IrqScope,
    ) -> Result<LineStateSnapshot, IrqError> {
        let mut snapshot = LineStateSnapshot::new(scope);
        match scope {
            IrqScope::Global => {
                snapshot.global = self.snapshot_and_disable_line(irq, None)?;
            }
            IrqScope::PerCpu { cpus } => {
                for cpu in cpus.iter() {
                    if !self.ops.cpu_online(cpu) {
                        continue;
                    }
                    match self.snapshot_and_disable_line(irq, Some(cpu)) {
                        Ok(was_enabled) => snapshot.percpu.push((cpu, was_enabled)),
                        Err(err) => {
                            let _ = self.restore_scope_line_snapshot(irq, scope, &snapshot);
                            return Err(err);
                        }
                    }
                }
            }
        }
        Ok(snapshot)
    }

    fn snapshot_and_disable_line(
        &self,
        irq: IrqNumber,
        cpu: Option<CpuId>,
    ) -> Result<bool, IrqError> {
        let was_enabled = self.controller_line_enabled(irq, cpu)?;
        self.set_controller_enabled(irq, cpu, false)?;
        self.set_line_applied_if_present(irq, cpu, false)?;
        Ok(was_enabled)
    }

    fn restore_scope_line_snapshot(
        &self,
        irq: IrqNumber,
        scope: IrqScope,
        snapshot: &LineStateSnapshot,
    ) -> Result<(), IrqError> {
        match scope {
            IrqScope::Global => {
                self.restore_line_snapshot(irq, None, snapshot.global)?;
            }
            IrqScope::PerCpu { cpus } => {
                for cpu in cpus.iter() {
                    if let Some((_, was_enabled)) = snapshot
                        .percpu
                        .iter()
                        .find(|(snapshot_cpu, _)| *snapshot_cpu == cpu)
                    {
                        self.restore_line_snapshot(irq, Some(cpu), *was_enabled)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn restore_line_snapshot(
        &self,
        irq: IrqNumber,
        cpu: Option<CpuId>,
        was_enabled: bool,
    ) -> Result<(), IrqError> {
        if was_enabled {
            self.set_controller_enabled(irq, cpu, true)?;
        }
        self.set_line_applied_if_present(irq, cpu, was_enabled)?;
        Ok(())
    }

    fn controller_line_enabled(
        &self,
        irq: IrqNumber,
        cpu: Option<CpuId>,
    ) -> Result<bool, IrqError> {
        match self.ops.is_enabled(irq, cpu) {
            Ok(enabled) => Ok(enabled),
            Err(IrqError::Unsupported) => {
                Ok(self.framework_line_enabled(irq, cpu).unwrap_or(false))
            }
            Err(err) => Err(err),
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

    fn set_line_applied_if_present(
        &self,
        irq: IrqNumber,
        cpu: Option<CpuId>,
        enabled: bool,
    ) -> Result<(), IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = {
            let state = unsafe { &mut *self.state.get() };
            if let Some(descriptor) = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == irq)
            {
                descriptor.set_line_applied(cpu, enabled);
            }
            Ok(())
        };
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

struct LineStateSnapshot {
    global: bool,
    percpu: Vec<(CpuId, bool)>,
}

impl LineStateSnapshot {
    fn new(scope: IrqScope) -> Self {
        Self {
            global: false,
            percpu: match scope {
                IrqScope::Global => Vec::new(),
                IrqScope::PerCpu { cpus } => Vec::with_capacity(cpus.iter().count()),
            },
        }
    }
}

struct DispatchGuard<'a, O: IrqOps> {
    registry: &'a Registry<O>,
    irq: IrqNumber,
}

struct ActionRunGuard<'a> {
    action: &'a Action,
}

impl<'a> ActionRunGuard<'a> {
    fn enter(action: &'a Action) -> Option<Self> {
        match action.execution {
            IrqExecution::Concurrent => Some(Self { action }),
            IrqExecution::NonReentrant => action
                .running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .ok()
                .map(|_| Self { action }),
        }
    }
}

impl Drop for ActionRunGuard<'_> {
    fn drop(&mut self) {
        if self.action.execution == IrqExecution::NonReentrant {
            self.action.running.store(false, Ordering::Release);
        }
    }
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

fn status_cpu(scope: IrqScope, current: CpuId) -> Option<CpuId> {
    match scope {
        IrqScope::Global => None,
        IrqScope::PerCpu { .. } => Some(current),
    }
}

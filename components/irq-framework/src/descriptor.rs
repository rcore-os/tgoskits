use core::{
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{
    CpuId, CpuMask, IrqAffinity, IrqError, IrqExecution, IrqId, IrqLineBinding, IrqLineControl,
    IrqRequest, IrqScope, PreparedIrqLine, ShareMode, action::Action,
    detached::DetachedActionConfig, lock::MetadataLock,
};

pub(crate) struct Descriptor {
    pub(crate) irq: IrqId,
    /// Serializes all mutable descriptor state and irqchip transitions.
    ///
    /// Descriptors are pinned by the registry for its complete lifetime, so
    /// task-side, hard-IRQ, and target-CPU transitions can safely rendezvous on
    /// this address without holding the registry catalog lock or serializing
    /// unrelated devices. The protected irqchip leaf is bounded and
    /// infallible, so desired/applied state commits at one linearization point.
    controller_lock: MetadataLock,
    binding_state: LineBindingState,
    share_mode: ShareMode,
    affinity: IrqAffinity,
    line_scope: LineScope,
    registration_held: bool,
    global_claims: usize,
    percpu_claims: [u16; u128::BITS as usize],
    /// Pins the action list for dispatch readers and asynchronous drain
    /// notifications. Controller EOI ordering is tracked separately by the
    /// scope-specific claim counters above.
    pub(crate) in_flight: AtomicUsize,
    line_desired: bool,
    line_applied: bool,
    percpu_line_desired: CpuMask,
    percpu_line_applied: CpuMask,
    percpu_line_initialized: CpuMask,
    pub(crate) head: *mut Action,
}

impl Descriptor {
    pub(crate) fn new(irq: IrqId, request: &IrqRequest) -> Self {
        Self::new_with_config(irq, request.scope, request.share_mode, request.affinity)
    }

    pub(crate) fn new_with_config(
        irq: IrqId,
        scope: IrqScope,
        share_mode: ShareMode,
        affinity: IrqAffinity,
    ) -> Self {
        Self {
            irq,
            controller_lock: MetadataLock::new(),
            binding_state: LineBindingState::Unbound,
            share_mode,
            affinity,
            line_scope: LineScope::from_scope(scope),
            registration_held: false,
            global_claims: 0,
            percpu_claims: [0; u128::BITS as usize],
            in_flight: AtomicUsize::new(0),
            line_desired: false,
            line_applied: false,
            percpu_line_desired: CpuMask::empty(),
            percpu_line_applied: CpuMask::empty(),
            percpu_line_initialized: CpuMask::empty(),
            head: ptr::null_mut(),
        }
    }

    pub(crate) const fn controller_lock(&self) -> &MetadataLock {
        &self.controller_lock
    }

    pub(crate) fn install_line_binding(&mut self, prepared: PreparedIrqLine) {
        assert!(
            self.registration_held && self.binding_state == LineBindingState::Unbound,
            "IRQ line binding installation lost its registration reservation"
        );
        self.binding_state = LineBindingState::Installing(prepared);
    }

    pub(crate) const fn line_binding(&self) -> Option<IrqLineBinding> {
        match self.binding_state {
            LineBindingState::Bound(prepared) => Some(prepared.binding()),
            LineBindingState::Unbound
            | LineBindingState::Installing(_)
            | LineBindingState::Releasing(_) => None,
        }
    }

    pub(crate) const fn preparing_binding(&self) -> Option<IrqLineBinding> {
        match self.binding_state {
            LineBindingState::Installing(prepared)
            | LineBindingState::Bound(prepared)
            | LineBindingState::Releasing(prepared) => Some(prepared.binding()),
            LineBindingState::Unbound => None,
        }
    }

    pub(crate) fn mark_line_prepared(&mut self, prepared: PreparedIrqLine) {
        assert_eq!(
            self.binding_state,
            LineBindingState::Installing(prepared),
            "IRQ line preparation committed another binding generation"
        );
        self.binding_state = LineBindingState::Bound(prepared);
    }

    pub(crate) const fn line_control(&self) -> Option<IrqLineControl> {
        match self.binding_state {
            LineBindingState::Installing(prepared)
            | LineBindingState::Bound(prepared)
            | LineBindingState::Releasing(prepared) => Some(prepared.control()),
            LineBindingState::Unbound => None,
        }
    }

    pub(crate) const fn affinity(&self) -> IrqAffinity {
        self.affinity
    }

    pub(crate) fn compatible_with(&mut self, request: &IrqRequest) -> Result<(), IrqError> {
        self.compatible_with_config(request.scope, request.share_mode, request.affinity)
    }

    pub(crate) fn compatible_with_detached(
        &mut self,
        config: DetachedActionConfig,
    ) -> Result<(), IrqError> {
        self.compatible_with_config(config.scope, config.share_mode, config.affinity)
    }

    fn compatible_with_config(
        &mut self,
        scope: IrqScope,
        share_mode: ShareMode,
        affinity: IrqAffinity,
    ) -> Result<(), IrqError> {
        if self.registration_held || matches!(self.binding_state, LineBindingState::Releasing(_)) {
            return Err(IrqError::Busy);
        }
        let mut has_active_actions = false;
        for action in self.actions() {
            let action = unsafe { &*action };
            if action.detached.load(Ordering::Acquire) {
                continue;
            }
            has_active_actions = true;
            if !scope_compatible(action.scope, scope) {
                return Err(IrqError::InvalidIrq);
            }
        }

        if !has_active_actions && self.binding_state == LineBindingState::Unbound {
            self.share_mode = share_mode;
            self.affinity = affinity;
            self.line_scope = LineScope::from_scope(scope);
            return Ok(());
        }

        if !self.line_scope.accepts(scope) {
            return Err(IrqError::InvalidIrq);
        }
        if self.affinity != affinity && affinity != IrqAffinity::Any {
            return Err(IrqError::Busy);
        }
        if !has_active_actions {
            self.share_mode = share_mode;
            return Ok(());
        }

        if self.share_mode != ShareMode::Shared || share_mode != ShareMode::Shared {
            return Err(IrqError::Busy);
        }

        // Affinity belongs to the backing line, not to an individual shared
        // action. `Any` is therefore an unconstrained join request and
        // inherits an existing fixed route. Changing an already-published
        // `Any` line to `Fixed`, or joining two different fixed routes,
        // remains a separate controller transaction and is rejected here.
        if self.affinity != affinity && affinity != IrqAffinity::Any {
            return Err(IrqError::Busy);
        }

        Ok(())
    }

    pub(crate) const fn detached_config(
        &self,
        scope: IrqScope,
        execution: IrqExecution,
    ) -> DetachedActionConfig {
        DetachedActionConfig {
            irq: self.irq,
            scope,
            affinity: self.affinity,
            execution,
            share_mode: self.share_mode,
        }
    }

    pub(crate) fn actions(&self) -> ActionIter {
        ActionIter { next: self.head }
    }

    pub(crate) fn line_desired(&self, cpu: Option<CpuId>) -> bool {
        match cpu {
            Some(cpu) => self.percpu_line_desired.contains(cpu),
            None => self.line_desired,
        }
    }

    pub(crate) fn line_applied(&self, cpu: Option<CpuId>) -> bool {
        match cpu {
            Some(cpu) => self.percpu_line_applied.contains(cpu),
            None => self.line_applied,
        }
    }

    pub(crate) fn begin_registration(&mut self, request: &IrqRequest) -> Result<bool, IrqError> {
        self.compatible_with(request)?;
        self.registration_held = true;
        match self.binding_state {
            LineBindingState::Unbound => Ok(true),
            LineBindingState::Bound(_) => Ok(false),
            LineBindingState::Releasing(_) => Err(IrqError::Busy),
            LineBindingState::Installing(_) => {
                panic!("IRQ descriptor retained an interrupted binding installation")
            }
        }
    }

    pub(crate) fn begin_detached_registration(
        &mut self,
        config: DetachedActionConfig,
    ) -> Result<bool, IrqError> {
        self.compatible_with_detached(config)?;
        self.registration_held = true;
        match self.binding_state {
            LineBindingState::Unbound => Ok(true),
            LineBindingState::Bound(_) => Ok(false),
            LineBindingState::Releasing(_) => Err(IrqError::Busy),
            LineBindingState::Installing(_) => {
                panic!("IRQ descriptor retained an interrupted line transaction")
            }
        }
    }

    pub(crate) const fn registration_held(&self) -> bool {
        self.registration_held
    }

    pub(crate) fn finish_registration(&mut self) {
        debug_assert!(self.registration_held);
        self.registration_held = false;
    }

    pub(crate) const fn line_release_reserved(&self) -> bool {
        matches!(self.binding_state, LineBindingState::Releasing(_))
    }

    pub(crate) const fn line_accepts_action_transition(&self) -> bool {
        matches!(self.binding_state, LineBindingState::Bound(_))
    }

    pub(crate) fn begin_line_release(
        &mut self,
        handle_id: u64,
    ) -> Result<(PreparedIrqLine, DetachedActionConfig, *mut Action), IrqError> {
        if self.registration_held || !matches!(self.line_scope, LineScope::Global) {
            return Err(IrqError::Busy);
        }
        let LineBindingState::Bound(prepared) = self.binding_state else {
            return Err(IrqError::Busy);
        };
        if prepared.control() != IrqLineControl::Maskable
            || self.line_desired
            || self.line_applied
            || self.global_claims != 0
            || self.in_flight.load(Ordering::Acquire) != 0
        {
            return Err(IrqError::Busy);
        }

        let mut actions = self
            .actions()
            .filter(|action| unsafe { !(**action).detached.load(Ordering::Acquire) });
        let action = actions.next().ok_or(IrqError::NotFound)?;
        if actions.next().is_some() {
            return Err(IrqError::Busy);
        }
        let action = unsafe { &*action };
        if action.id != handle_id {
            return Err(IrqError::NotFound);
        }
        if !action.is_detachable() {
            return Err(IrqError::Busy);
        }

        let config = self.detached_config(action.scope, action.execution);
        let action = action as *const Action as *mut Action;
        self.binding_state = LineBindingState::Releasing(prepared);
        Ok((prepared, config, action))
    }

    pub(crate) fn rollback_line_release(&mut self, prepared: PreparedIrqLine) {
        assert_eq!(
            self.binding_state,
            LineBindingState::Releasing(prepared),
            "IRQ line release rollback lost its binding reservation"
        );
        self.binding_state = LineBindingState::Bound(prepared);
    }

    pub(crate) fn finish_line_release(&mut self, prepared: PreparedIrqLine) {
        assert_eq!(
            self.binding_state,
            LineBindingState::Releasing(prepared),
            "IRQ line release commit lost its binding reservation"
        );
        assert!(
            !self.registration_held
                && self.head.is_null()
                && self.global_claims == 0
                && self.in_flight.load(Ordering::Acquire) == 0
                && !self.line_desired
                && !self.line_applied,
            "released IRQ line retained framework ownership"
        );
        self.binding_state = LineBindingState::Unbound;
        self.percpu_line_desired = CpuMask::empty();
        self.percpu_line_applied = CpuMask::empty();
        self.percpu_line_initialized = CpuMask::empty();
    }

    pub(crate) fn begin_irq_claim(&mut self, cpu: CpuId) {
        assert!(
            matches!(self.binding_state, LineBindingState::Bound(_)),
            "IRQ dispatch reached a descriptor without a prepared line binding"
        );
        match self.line_scope {
            LineScope::Global => {
                if let IrqAffinity::Fixed(owner) = self.affinity {
                    assert_eq!(
                        cpu, owner,
                        "fixed IRQ line was delivered outside its canonical owner CPU"
                    );
                }
                self.global_claims = self
                    .global_claims
                    .checked_add(1)
                    .expect("global IRQ claim count overflowed");
            }
            LineScope::PerCpu(cpus) => {
                assert!(
                    cpus.contains(cpu),
                    "per-CPU IRQ line was delivered outside its canonical CPU mask"
                );
                let claims = self
                    .percpu_claims
                    .get_mut(cpu.0)
                    .expect("validated per-CPU IRQ id exceeded claim storage");
                *claims = claims
                    .checked_add(1)
                    .expect("per-CPU IRQ claim count overflowed");
            }
        }
    }

    pub(crate) const fn dispatchable(&self) -> bool {
        matches!(self.binding_state, LineBindingState::Bound(_))
    }

    pub(crate) fn end_irq_claim(&mut self, cpu: CpuId) -> Option<CpuId> {
        match self.line_scope {
            LineScope::Global => {
                self.global_claims = self
                    .global_claims
                    .checked_sub(1)
                    .expect("global IRQ claim count underflowed");
                None
            }
            LineScope::PerCpu(_) => {
                let claims = self
                    .percpu_claims
                    .get_mut(cpu.0)
                    .expect("per-CPU IRQ claim ended on an invalid CPU");
                *claims = claims
                    .checked_sub(1)
                    .expect("per-CPU IRQ claim count underflowed");
                Some(cpu)
            }
        }
    }

    pub(crate) fn line_claims(&self, cpu: Option<CpuId>) -> usize {
        match (self.line_scope, cpu) {
            (LineScope::Global, None) => self.global_claims,
            (LineScope::PerCpu(_), Some(cpu)) => self
                .percpu_claims
                .get(cpu.0)
                .copied()
                .map(usize::from)
                .unwrap_or(0),
            _ => 0,
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

    pub(crate) fn set_line_applied(&mut self, cpu: Option<CpuId>, enabled: bool) {
        match cpu {
            Some(cpu) => {
                self.percpu_line_initialized.insert(cpu);
                if enabled {
                    self.percpu_line_applied.insert(cpu);
                } else {
                    self.percpu_line_applied.remove(cpu);
                }
            }
            None => self.line_applied = enabled,
        }
    }

    pub(crate) fn recompute_line_desired(&mut self, cpu: Option<CpuId>) {
        let line_owned = self.actions().any(|action| {
            let action = unsafe { &*action };
            !action.detached.load(Ordering::Acquire) && action.quench_applies(cpu)
        });
        let desired = !line_owned
            && self.actions().any(|action| {
                let action = unsafe { &*action };
                !action.detached.load(Ordering::Acquire)
                    && action.enabled_on(cpu)
                    && cpu.is_none_or(|cpu| action_matches_cpu(action.scope, cpu))
            });
        self.set_line_desired(cpu, desired);
    }

    pub(crate) fn line_initialized(&self, cpu: Option<CpuId>) -> bool {
        match (self.line_scope, cpu) {
            (LineScope::Global, None) => self.line_binding().is_some(),
            (LineScope::PerCpu(cpus), Some(cpu)) => {
                cpus.contains(cpu) && self.percpu_line_initialized.contains(cpu)
            }
            _ => false,
        }
    }

    pub(crate) fn cpu_online_work(&self, cpu: CpuId) -> Option<(IrqLineBinding, bool)> {
        let LineScope::PerCpu(cpus) = self.line_scope else {
            return None;
        };
        if !cpus.contains(cpu) {
            return None;
        }
        let binding = self.line_binding()?;
        let needs_initialization = !self.percpu_line_initialized.contains(cpu);
        (needs_initialization || self.line_desired(Some(cpu)) != self.line_applied(Some(cpu)))
            .then_some((binding, needs_initialization))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LineScope {
    Global,
    PerCpu(CpuMask),
}

impl LineScope {
    const fn from_scope(scope: IrqScope) -> Self {
        match scope {
            IrqScope::Global => Self::Global,
            IrqScope::PerCpu { cpus } => Self::PerCpu(cpus),
        }
    }

    fn accepts(self, scope: IrqScope) -> bool {
        match (self, scope) {
            (Self::Global, IrqScope::Global) => true,
            (Self::PerCpu(canonical), IrqScope::PerCpu { cpus }) => canonical == cpus,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LineBindingState {
    Unbound,
    Installing(PreparedIrqLine),
    Bound(PreparedIrqLine),
    Releasing(PreparedIrqLine),
}

pub(crate) struct ActionIter {
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

pub(crate) fn action_matches_cpu(scope: IrqScope, cpu: CpuId) -> bool {
    match scope {
        IrqScope::Global => true,
        IrqScope::PerCpu { cpus } => cpus.contains(cpu),
    }
}

fn scope_compatible(existing: IrqScope, requested: IrqScope) -> bool {
    matches!(
        (existing, requested),
        (IrqScope::Global, IrqScope::Global) | (IrqScope::PerCpu { .. }, IrqScope::PerCpu { .. })
    )
}

pub(crate) fn recompute_scope_line_desired(descriptor: &mut Descriptor, scope: IrqScope) {
    match scope {
        IrqScope::Global => descriptor.recompute_line_desired(None),
        IrqScope::PerCpu { cpus } => {
            for cpu in cpus.iter() {
                descriptor.recompute_line_desired(Some(cpu));
            }
        }
    }
}

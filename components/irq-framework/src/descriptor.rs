use core::{
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{
    CpuId, CpuMask, IrqAffinity, IrqError, IrqExecution, IrqId, IrqRequest, IrqScope, ShareMode,
    action::Action, detached::DetachedActionConfig,
};

pub(crate) struct Descriptor {
    pub(crate) irq: IrqId,
    share_mode: ShareMode,
    affinity: IrqAffinity,
    pub(crate) in_flight: AtomicUsize,
    line_desired: bool,
    line_applied: bool,
    percpu_line_desired: CpuMask,
    percpu_line_applied: CpuMask,
    pub(crate) head: *mut Action,
}

impl Descriptor {
    pub(crate) fn new(irq: IrqId, request: &IrqRequest) -> Self {
        Self::new_with_config(irq, request.share_mode, request.affinity)
    }

    pub(crate) fn new_with_config(
        irq: IrqId,
        share_mode: ShareMode,
        affinity: IrqAffinity,
    ) -> Self {
        Self {
            irq,
            share_mode,
            affinity,
            in_flight: AtomicUsize::new(0),
            line_desired: false,
            line_applied: false,
            percpu_line_desired: CpuMask::empty(),
            percpu_line_applied: CpuMask::empty(),
            head: ptr::null_mut(),
        }
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

        if !has_active_actions {
            self.share_mode = share_mode;
            self.affinity = affinity;
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
            !action.detached.load(Ordering::Acquire)
                && (action.quench_applies(cpu) || action.has_continuation())
        });
        let desired = !line_owned
            && self.actions().any(|action| {
                let action = unsafe { &*action };
                !action.detached.load(Ordering::Acquire)
                    && action.enabled()
                    && cpu.is_none_or(|cpu| action_matches_cpu(action.scope, cpu))
            });
        self.set_line_desired(cpu, desired);
    }
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

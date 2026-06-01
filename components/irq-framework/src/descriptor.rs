use core::{
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{CpuId, CpuMask, IrqError, IrqNumber, IrqRequest, IrqScope, ShareMode, action::Action};

pub(crate) struct Descriptor {
    pub(crate) irq: IrqNumber,
    share_mode: ShareMode,
    pub(crate) in_flight: AtomicUsize,
    line_desired: bool,
    line_applied: bool,
    percpu_line_desired: CpuMask,
    percpu_line_applied: CpuMask,
    pub(crate) head: *mut Action,
}

impl Descriptor {
    pub(crate) fn new(irq: IrqNumber, request: &IrqRequest) -> Self {
        Self {
            irq,
            share_mode: request.share_mode,
            in_flight: AtomicUsize::new(0),
            line_desired: false,
            line_applied: false,
            percpu_line_desired: CpuMask::empty(),
            percpu_line_applied: CpuMask::empty(),
            head: ptr::null_mut(),
        }
    }

    pub(crate) fn compatible_with(&mut self, request: &IrqRequest) -> Result<(), IrqError> {
        let has_active_actions = self.actions().any(|action| {
            let action = unsafe { &*action };
            !action.detached.load(Ordering::Acquire)
        });

        if !has_active_actions {
            self.share_mode = request.share_mode;
            return Ok(());
        }

        if self.share_mode != ShareMode::Shared || request.share_mode != ShareMode::Shared {
            return Err(IrqError::Busy);
        }

        Ok(())
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

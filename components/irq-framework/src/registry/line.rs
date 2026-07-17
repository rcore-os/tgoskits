//! Controller line state and per-CPU enable coordination.

use alloc::vec::Vec;

use super::Registry;
use crate::{
    CpuId, IrqAffinity, IrqError, IrqHandle, IrqId, IrqOps, IrqScope,
    descriptor::action_matches_cpu,
};

impl<O: IrqOps> Registry<O> {
    pub(super) fn apply_enabled(
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

    pub(super) fn apply_affinity(&self, irq: IrqId, affinity: IrqAffinity) -> Result<(), IrqError> {
        match affinity {
            IrqAffinity::Any => Ok(()),
            IrqAffinity::Fixed(cpu) if self.ops.cpu_online(cpu) => {
                self.ops.set_affinity(irq, affinity)
            }
            IrqAffinity::Fixed(_) => Err(IrqError::CpuOffline),
        }
    }

    pub(super) fn snapshot_and_disable_scope_line(
        &self,
        irq: IrqId,
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

    pub(super) fn restore_scope_line_snapshot(
        &self,
        irq: IrqId,
        scope: IrqScope,
        snapshot: &LineStateSnapshot,
    ) -> Result<(), IrqError> {
        match scope {
            IrqScope::Global => self.restore_line_snapshot(irq, None, snapshot.global),
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
                Ok(())
            }
        }
    }

    pub(super) fn apply_line_state(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<(), IrqError> {
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

    pub(super) fn pending_enables_for_cpu(&self, cpu: CpuId) -> Vec<IrqId> {
        let irq_state = self.lock.lock(&self.ops);
        let mut pending = Vec::new();
        for descriptor in &self.state_ref().descriptors {
            if descriptor.actions().any(|action| {
                let action = unsafe { &*action };
                !action.detached.load(core::sync::atomic::Ordering::Acquire)
                    && action.pending_enable_contains(cpu)
                    && action_matches_cpu(action.scope, cpu)
            }) {
                pending.push(descriptor.irq);
            }
        }
        self.lock.unlock(&self.ops, irq_state);
        pending
    }

    pub(super) fn clear_pending_enable_for_cpu(&self, irq: IrqId, cpu: CpuId) {
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

    pub(super) fn framework_line_enabled(
        &self,
        irq: IrqId,
        cpu: Option<CpuId>,
    ) -> Result<bool, IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let descriptor = self.descriptor(irq).ok_or(IrqError::NotFound)?;
            Ok(descriptor.line_applied(cpu))
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
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

    fn snapshot_and_disable_line(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<bool, IrqError> {
        let was_enabled = self.controller_line_enabled(irq, cpu)?;
        self.set_controller_enabled(irq, cpu, false)?;
        self.set_line_applied_if_present(irq, cpu, false)?;
        Ok(was_enabled)
    }

    fn restore_line_snapshot(
        &self,
        irq: IrqId,
        cpu: Option<CpuId>,
        was_enabled: bool,
    ) -> Result<(), IrqError> {
        if was_enabled {
            self.set_controller_enabled(irq, cpu, true)?;
        }
        self.set_line_applied_if_present(irq, cpu, was_enabled)
    }

    fn controller_line_enabled(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<bool, IrqError> {
        match self.ops.is_enabled(irq, cpu) {
            Ok(enabled) => Ok(enabled),
            Err(IrqError::Unsupported) => {
                Ok(self.framework_line_enabled(irq, cpu).unwrap_or(false))
            }
            Err(err) => Err(err),
        }
    }

    fn set_controller_enabled(
        &self,
        irq: IrqId,
        cpu: Option<CpuId>,
        enabled: bool,
    ) -> Result<(), IrqError> {
        match cpu {
            None => self.ops.set_enabled(irq, None, enabled),
            Some(cpu) => {
                let mut request = CpuOwnedLineUpdate {
                    registry: self as *const Self as *mut (),
                    irq,
                    cpu,
                    enabled,
                    result: Ok(()),
                };
                self.ops.run_on_cpu_sync(
                    cpu,
                    cpu_owned_line_update_thunk::<O>,
                    (&mut request as *mut CpuOwnedLineUpdate).cast(),
                )?;
                request.result
            }
        }
    }

    fn line_state(&self, irq: IrqId, cpu: Option<CpuId>) -> Option<(bool, bool)> {
        let irq_state = self.lock.lock(&self.ops);
        let result = self
            .descriptor(irq)
            .map(|descriptor| (descriptor.line_desired(cpu), descriptor.line_applied(cpu)));
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn set_line_applied(
        &self,
        irq: IrqId,
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
        irq: IrqId,
        cpu: Option<CpuId>,
        enabled: bool,
    ) -> Result<(), IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let state = unsafe { &mut *self.state.get() };
        if let Some(descriptor) = state
            .descriptors
            .iter_mut()
            .find(|descriptor| descriptor.irq == irq)
        {
            descriptor.set_line_applied(cpu, enabled);
        }
        self.lock.unlock(&self.ops, irq_state);
        Ok(())
    }
}

pub(super) struct LineStateSnapshot {
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

struct CpuOwnedLineUpdate {
    registry: *mut (),
    irq: IrqId,
    cpu: CpuId,
    enabled: bool,
    result: Result<(), IrqError>,
}

unsafe fn cpu_owned_line_update_thunk<O: IrqOps>(arg: *mut ()) {
    let request = unsafe { &mut *arg.cast::<CpuOwnedLineUpdate>() };
    let registry = unsafe { &*(request.registry as *const Registry<O>) };
    request.result = registry
        .ops
        .set_enabled(request.irq, Some(request.cpu), request.enabled);
}

//! Prepared IRQ-chip line state and per-CPU enable coordination.

use alloc::{boxed::Box, vec::Vec};

use super::Registry;
use crate::{
    CpuId, IrqError, IrqHandle, IrqId, IrqLineBinding, IrqLineControl, IrqOps, IrqRequest,
    IrqScope, PreparedIrqLine, descriptor::Descriptor, detached::DetachedActionConfig,
};

impl<O: IrqOps> Registry<O> {
    pub(super) fn begin_line_registration(
        &self,
        irq: IrqId,
        request: &IrqRequest,
    ) -> Result<bool, IrqError> {
        if self.descriptor_ptr(irq).is_some() {
            return self.with_descriptor(irq, |descriptor| descriptor.begin_registration(request));
        }

        // Allocate before taking the writer-only catalog lock. A concurrent
        // registrar may publish the same descriptor first; in that case this
        // unused candidate is dropped in task context after the lock releases.
        let mut candidate = Box::pin(Descriptor::new(irq, request));
        let irq_state = self.catalog_lock.lock(&self.ops);
        let result = (|| {
            if self.descriptor_ptr(irq).is_some() {
                return Ok(None);
            }
            let state = unsafe { &mut *self.state.get() };
            if state.retained.len() == state.retained.capacity() {
                return Err(IrqError::NoMemory);
            }
            let slot = self.vacant_descriptor_slot(irq).ok_or(IrqError::NoMemory)?;
            let needs_prepare = candidate.as_mut().get_mut().begin_registration(request)?;
            debug_assert!(needs_prepare);
            let descriptor = candidate.as_mut().get_mut() as *mut Descriptor;
            state.retained.push(candidate);
            self.descriptor_catalog[slot].store(descriptor, core::sync::atomic::Ordering::Release);
            Ok(Some(needs_prepare))
        })();
        self.catalog_lock.unlock(&self.ops, irq_state);
        match result? {
            Some(needs_prepare) => Ok(needs_prepare),
            None => self.with_descriptor(irq, |descriptor| descriptor.begin_registration(request)),
        }
    }

    /// Resolves and physically masks a descriptor before its first action is
    /// published. All fallible platform work happens before the binding is
    /// committed; a failure leaves the descriptor unbound and retryable.
    pub(super) fn prepare_registration_line(
        &self,
        irq: IrqId,
        request: &IrqRequest,
        needs_prepare: bool,
    ) -> Result<(), IrqError> {
        self.prepare_line_for_registration(irq, request.scope, request.affinity, needs_prepare)
    }

    pub(super) fn begin_detached_line_registration(
        &self,
        config: DetachedActionConfig,
    ) -> Result<bool, IrqError> {
        self.with_descriptor(config.irq, |descriptor| {
            descriptor.begin_detached_registration(config)
        })
    }

    pub(super) fn prepare_detached_registration_line(
        &self,
        config: DetachedActionConfig,
        needs_prepare: bool,
    ) -> Result<(), IrqError> {
        self.prepare_line_for_registration(config.irq, config.scope, config.affinity, needs_prepare)
    }

    fn prepare_line_for_registration(
        &self,
        irq: IrqId,
        scope: IrqScope,
        affinity: crate::IrqAffinity,
        needs_prepare: bool,
    ) -> Result<(), IrqError> {
        if !needs_prepare {
            return Ok(());
        }

        let prepared = self.ops.prepare_line(irq, scope, affinity)?;
        let binding = prepared.binding();
        self.install_line_binding(irq, prepared)?;

        match scope {
            IrqScope::Global => {
                // `prepare_line` returns a globally masked endpoint.
                self.set_line_applied(irq, None, false)?;
            }
            IrqScope::PerCpu { cpus } => match prepared.control() {
                IrqLineControl::Maskable => {
                    for cpu in cpus.iter() {
                        if self.ops.cpu_online(cpu)
                            && let Err(error) = self.mask_prepared_percpu_line(irq, binding, cpu)
                        {
                            panic!(
                                "prepared per-CPU IRQ line {irq:?} could not be masked on online \
                                 CPU {}: {error:?}",
                                cpu.0
                            );
                        }
                    }
                }
                IrqLineControl::ActionGateOnly => {
                    for cpu in cpus.iter() {
                        self.set_line_applied(irq, Some(cpu), false)?;
                    }
                }
            },
        }
        self.mark_line_prepared(irq, prepared)
    }

    pub(super) fn finish_line_registration(&self, irq: IrqId) -> Result<(), IrqError> {
        self.with_descriptor(irq, |descriptor| {
            if !descriptor.registration_held() {
                return Err(IrqError::Busy);
            }
            descriptor.finish_registration();
            Ok(())
        })
    }

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

    pub(super) fn apply_line_state(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<(), IrqError> {
        match cpu {
            None => match self.line_affinity(irq)? {
                crate::IrqAffinity::Any => self.reconcile_line_local(irq, None),
                crate::IrqAffinity::Fixed(owner_cpu) => {
                    if !self.line_update_required(irq, None)? {
                        return Ok(());
                    }
                    self.run_line_update_on(owner_cpu, irq, None)
                }
            },
            Some(cpu) => {
                if !self.line_update_required(irq, Some(cpu))? {
                    return Ok(());
                }
                self.run_line_update_on(cpu, irq, Some(cpu))
            }
        }
    }

    fn run_line_update_on(
        &self,
        owner_cpu: CpuId,
        irq: IrqId,
        line_cpu: Option<CpuId>,
    ) -> Result<(), IrqError> {
        let mut request = CpuOwnedLineUpdate {
            registry: self as *const Self as *mut (),
            irq,
            line_cpu,
            result: Ok(()),
        };
        if let Err(error) = self.ops.run_on_cpu_sync(
            owner_cpu,
            cpu_owned_line_update_thunk::<O>,
            (&mut request as *mut CpuOwnedLineUpdate).cast(),
        ) {
            panic!(
                "prepared IRQ line {irq:?} could not execute on owner CPU {}: {error:?}",
                owner_cpu.0
            );
        }
        request.result
    }

    fn reconcile_line_local(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<(), IrqError> {
        if let Some(cpu) = cpu
            && !self.ops.cpu_online(cpu)
        {
            return Ok(());
        }

        self.with_descriptor(irq, |descriptor| {
            assert!(
                descriptor.line_initialized(cpu),
                "live IRQ line transition reached an uninitialized CPU instance"
            );
            let desired = descriptor.line_desired(cpu);
            if desired == descriptor.line_applied(cpu)
                || (desired && descriptor.line_claims(cpu) != 0)
            {
                return Ok(());
            }
            match descriptor
                .line_control()
                .expect("live IRQ transition lost its control mode")
            {
                IrqLineControl::Maskable => {
                    let binding = descriptor
                        .line_binding()
                        .expect("live IRQ transition lost its prepared binding");
                    self.ops.set_line_enabled(binding, cpu, desired);
                }
                IrqLineControl::ActionGateOnly => {}
            }
            descriptor.set_line_applied(cpu, desired);
            Ok(())
        })
    }

    fn line_update_required(&self, irq: IrqId, cpu: Option<CpuId>) -> Result<bool, IrqError> {
        if let Some(cpu) = cpu
            && !self.ops.cpu_online(cpu)
        {
            return Ok(false);
        }
        self.with_descriptor(irq, |descriptor| {
            assert!(
                descriptor.line_initialized(cpu),
                "live IRQ line preflight reached an uninitialized CPU instance"
            );
            let desired = descriptor.line_desired(cpu);
            Ok(desired != descriptor.line_applied(cpu)
                && (!desired || descriptor.line_claims(cpu) == 0))
        })
    }

    pub(super) fn mask_prepared_percpu_line(
        &self,
        irq: IrqId,
        binding: IrqLineBinding,
        cpu: CpuId,
    ) -> Result<(), IrqError> {
        let mut request = CpuOwnedPreparedMask {
            registry: self as *const Self as *mut (),
            irq,
            binding,
            cpu,
            result: Ok(()),
        };
        self.ops.run_on_cpu_sync(
            cpu,
            cpu_owned_prepared_mask_thunk::<O>,
            (&mut request as *mut CpuOwnedPreparedMask).cast(),
        )?;
        request.result
    }

    fn mask_prepared_line_local(
        &self,
        irq: IrqId,
        binding: IrqLineBinding,
        cpu: CpuId,
    ) -> Result<(), IrqError> {
        self.with_descriptor(irq, |descriptor| {
            assert_eq!(
                descriptor.preparing_binding(),
                Some(binding),
                "per-CPU preparation used another binding generation"
            );
            self.ops.set_line_enabled(binding, Some(cpu), false);
            descriptor.set_line_applied(Some(cpu), false);
            Ok(())
        })
    }

    fn line_affinity(&self, irq: IrqId) -> Result<crate::IrqAffinity, IrqError> {
        self.with_descriptor(irq, |descriptor| Ok(descriptor.affinity()))
    }

    fn install_line_binding(&self, irq: IrqId, prepared: PreparedIrqLine) -> Result<(), IrqError> {
        self.with_descriptor(irq, |descriptor| {
            descriptor.install_line_binding(prepared);
            Ok(())
        })
    }

    fn mark_line_prepared(&self, irq: IrqId, prepared: PreparedIrqLine) -> Result<(), IrqError> {
        self.with_descriptor(irq, |descriptor| {
            descriptor.mark_line_prepared(prepared);
            Ok(())
        })
    }

    fn set_line_applied(
        &self,
        irq: IrqId,
        cpu: Option<CpuId>,
        enabled: bool,
    ) -> Result<(), IrqError> {
        self.with_descriptor(irq, |descriptor| {
            descriptor.set_line_applied(cpu, enabled);
            Ok(())
        })
    }

    pub(super) fn percpu_lines_for_cpu_online(
        &self,
        cpu: CpuId,
    ) -> Vec<(IrqId, IrqLineBinding, bool)> {
        let irq_state = self.catalog_lock.lock(&self.ops);
        let irqs = unsafe { &*self.state.get() }
            .retained
            .iter()
            .map(|descriptor| descriptor.irq)
            .collect::<Vec<_>>();
        self.catalog_lock.unlock(&self.ops, irq_state);

        let mut pending = Vec::new();
        for irq in irqs {
            if let Ok(Some((binding, needs_initialization))) =
                self.with_descriptor(irq, |descriptor| Ok(descriptor.cpu_online_work(cpu)))
            {
                pending.push((irq, binding, needs_initialization));
            }
        }
        pending
    }

    fn apply_percpu_enabled(
        &self,
        handle: IrqHandle,
        cpu: CpuId,
        _enabled: bool,
    ) -> Result<(), IrqError> {
        if self.ops.cpu_online(cpu) {
            self.apply_line_state(handle.irq, Some(cpu))?;
        }
        Ok(())
    }
}

struct CpuOwnedLineUpdate {
    registry: *mut (),
    irq: IrqId,
    line_cpu: Option<CpuId>,
    result: Result<(), IrqError>,
}

struct CpuOwnedPreparedMask {
    registry: *mut (),
    irq: IrqId,
    binding: IrqLineBinding,
    cpu: CpuId,
    result: Result<(), IrqError>,
}

unsafe fn cpu_owned_line_update_thunk<O: IrqOps>(arg: *mut ()) {
    let request = unsafe { &mut *arg.cast::<CpuOwnedLineUpdate>() };
    let registry = unsafe { &*(request.registry as *const Registry<O>) };
    request.result = registry.reconcile_line_local(request.irq, request.line_cpu);
}

unsafe fn cpu_owned_prepared_mask_thunk<O: IrqOps>(arg: *mut ()) {
    let request = unsafe { &mut *arg.cast::<CpuOwnedPreparedMask>() };
    let registry = unsafe { &*(request.registry as *const Registry<O>) };
    request.result = registry.mask_prepared_line_local(request.irq, request.binding, request.cpu);
}

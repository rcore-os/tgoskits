//! Core vCPU and nested-paging contract implemented by every target architecture.

use std::{format, sync::Arc, vec::Vec};

use ax_std::os::arceos::guard::IrqSaveGuard;
use axaddrspace::NestedPageTableOps;
use axvm_types::{VmArchPerCpuOps, VmArchVcpuOps, VmVcpuState};

use super::{BoundVcpuExit, VcpuRunAction};
use crate::{AxVmResult, ax_err, irq::model::PendingVcpuInterrupt};

pub(crate) trait ArchOps {
    type VCpu: VmArchVcpuOps;
    type PerCpu: VmArchPerCpuOps;
    type DeferredRunWork;
    type NestedPageTable: NestedPageTableOps;

    fn has_hardware_support() -> bool;

    #[allow(dead_code)]
    fn set_vcpu_on_args(vcpu: &crate::vm::AxVCpuRef<Self::VCpu>, _vcpu_id: usize, arg: usize) {
        vcpu.set_gpr(0, arg);
    }

    fn before_first_run(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {}

    /// Enters architecture-owned runtime state before the VM becomes runnable.
    fn enter_runtime(_vm: &crate::AxVM) -> AxVmResult {
        Ok(())
    }

    /// Leaves architecture-owned runtime state after stop or failed start.
    fn exit_runtime(_vm: &crate::AxVM) -> AxVmResult {
        Ok(())
    }

    /// Prepares task-owned architecture state for one vCPU run slice.
    ///
    /// This hook runs before the backend is loaded on a host CPU and may use
    /// sleepable task-context services. CPU-local state belongs in
    /// [`Self::before_vcpu_run`] instead.
    fn prepare_vcpu_run_slice(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        Ok(())
    }

    /// Prepares architecture state before each guest entry in a run slice.
    fn before_vcpu_run(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        Ok(())
    }

    /// Commits backend state staged by the preceding unbound exit handler.
    ///
    /// The hook runs immediately after the backend is loaded and before new
    /// interrupts are injected. It is the in-kernel equivalent of Linux KVM's
    /// `complete_userspace_io`: sleepable device work remains outside
    /// `vcpu_load()`/`vcpu_put()`, while the resulting RIP/register update is
    /// committed after the next `vcpu_load()`.
    fn complete_pending_vcpu_exit(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        Ok(())
    }

    fn after_vcpu_run(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {}

    fn wait_for_vcpu_event(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        runtime: &crate::vm::VmRuntimeHandle,
    ) {
        let wait_snapshot = runtime.vcpu_event_wait_snapshot();
        crate::vm::wait_for_vcpu_event_if_idle(
            runtime,
            &wait_snapshot,
            || vm.running(),
            || runtime.has_pending_interrupt(vcpu.id()),
            |condition| runtime.wait_until(condition),
        );
    }

    fn inject_arch_interrupt(
        vm_id: usize,
        vcpu: &crate::vcpu::AxVCpu<Self::VCpu>,
        interrupt: crate::runtime::QueuedVcpuInterrupt,
    ) {
        warn!(
            "VM[{}] VCpu[{}] dropped unsupported architecture interrupt {interrupt:?}",
            vm_id,
            vcpu.id()
        );
    }

    /// Injects a pending `PendingVcpuInterrupt` into the target vCPU.
    ///
    /// Called in the **target vCPU's run loop** so that accesses to banked
    /// system registers (GIC LR, x86 vLAPIC, etc.) happen on the correct
    /// physical CPU.
    fn inject_vcpu_interrupt(
        vcpu: &crate::vcpu::AxVCpu<Self::VCpu>,
        interrupt: PendingVcpuInterrupt,
    ) -> AxVmResult {
        vcpu.inject_interrupt_with_trigger(interrupt.id.0 as usize, interrupt.trigger)
    }

    /// Releases architecture runtime state after the VM's last vCPU exits.
    ///
    /// The VM reference is required for architecture state that is published
    /// through VM-local device services rather than indexed in global tables.
    fn on_last_vcpu_exit(vm: &crate::AxVMRef) -> AxVmResult {
        Self::exit_runtime(vm)
    }

    /// Handles a VM exit after the architecture backend has been unloaded.
    ///
    /// This hook may invoke sleepable runtime and device services. Any state
    /// update that requires a loaded backend must be staged here and committed
    /// by [`Self::complete_pending_vcpu_exit`] on the next bound entry.
    fn handle_vcpu_exit_unbound(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>>;

    fn finish_deferred_run_work(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxVmResult<VcpuRunAction>;

    fn run_vcpu(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult<VcpuRunAction>
    where
        Self: Sized,
    {
        let vm_id = vm.id();
        let vcpu_id = vcpu.id();
        let interrupt_owner = crate::host::task::current_thread().id().as_u64();

        match vcpu.state() {
            VmVcpuState::Free => vcpu.bind()?,
            VmVcpuState::Starting => vcpu.bind_after_cpu_on_or_rollback()?,
            VmVcpuState::Ready => {}
            state => {
                return ax_err!(
                    BadState,
                    format!("VCpu state is not Free or Ready, but {state:?}")
                );
            }
        }

        let run_result = run_vcpu_slice(
            || Self::prepare_vcpu_run_slice(vm, vcpu),
            || {
                vcpu.with_backend_bound_current_cpu(|| {
                    Self::complete_pending_vcpu_exit(vm, vcpu)?;

                    loop {
                        let interrupt_runtime = drain_and_inject_dispatched_interrupts::<Self>(
                            vm,
                            vcpu_id,
                            interrupt_owner,
                            vcpu,
                        );

                        // Device and forwarding work may acquire ordinary
                        // locks or call host IRQ services, so it must finish
                        // before the entry-only IRQ-disabled section.
                        Self::before_vcpu_run(vm, vcpu)?;

                        // Match Linux KVM's request/entry ordering: after the
                        // final queue check, keep local IRQs disabled through
                        // guest entry. A request published before this check is
                        // observed below; a later IPI remains pending until it
                        // forces a guest exit.
                        let entry_irq_guard = IrqSaveGuard::new();
                        if interrupt_runtime.as_ref().is_some_and(|runtime| {
                            runtime
                                .irq_dispatcher()
                                .has_pending(vcpu_id, interrupt_owner)
                        }) {
                            drop(entry_irq_guard);
                            continue;
                        }

                        let exit = vcpu.run_loaded();
                        Self::after_vcpu_run(vm, vcpu);
                        drop(entry_irq_guard);
                        break exit;
                    }
                })
            },
            |exit| {
                trace!("{exit:#x?}");
                Self::handle_vcpu_exit_unbound(vm, vcpu, exit)
            },
        );

        let unbind_result = vcpu.unbind();
        match run_result {
            Ok(BoundVcpuExit::Complete(action)) => {
                unbind_result?;
                Ok(action)
            }
            Ok(BoundVcpuExit::Defer(work)) => {
                unbind_result?;
                Self::finish_deferred_run_work(vm, vcpu, work)
            }
            Ok(BoundVcpuExit::DeferHypercall(work)) => {
                unbind_result?;
                let return_value = crate::runtime::hvc::finish_deferred_hypercall(vm.clone(), work);
                vcpu.set_return_value(return_value);
                Ok(VcpuRunAction {
                    waits_for_event: false,
                    stop_reason: None,
                    resets_vm: false,
                    exits_vcpu: false,
                })
            }
            Ok(BoundVcpuExit::Continue) => unreachable!("continued exits do not leave run loop"),
            Err(err) => {
                if let Err(unbind_err) = unbind_result {
                    warn!(
                        "VM[{vm_id}] VCpu[{vcpu_id}] unbind after run error failed: {unbind_err:?}"
                    );
                }
                Err(err)
            }
        }
    }
}

fn run_vcpu_slice<E, T>(
    prepare: impl FnOnce() -> AxVmResult,
    mut run_entry: impl FnMut() -> AxVmResult<E>,
    mut handle_exit: impl FnMut(E) -> AxVmResult<BoundVcpuExit<T>>,
) -> AxVmResult<BoundVcpuExit<T>> {
    prepare()?;
    loop {
        match handle_exit(run_entry()?)? {
            BoundVcpuExit::Continue => continue,
            action => return Ok(action),
        }
    }
}

fn drain_and_inject_dispatched_interrupts<A: ArchOps>(
    vm: &crate::AxVMRef,
    vcpu_id: usize,
    owner: u64,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
) -> Option<Arc<crate::vm::VmRuntimeHandle>> {
    let runtime = match vm.runtime_handle() {
        Ok(runtime) => runtime,
        Err(err) => {
            warn!(
                "VM[{}] VCpu[{}] cannot access interrupt dispatcher: {:?}",
                vm.id(),
                vcpu_id,
                err
            );
            return None;
        }
    };
    inject_drained_interrupts::<A>(runtime.irq_dispatcher(), vm.id(), vcpu_id, owner, vcpu);
    Some(runtime)
}

fn inject_drained_interrupts<A: ArchOps>(
    dispatcher: &crate::runtime::VcpuIrqDispatcher,
    vm_id: usize,
    vcpu_id: usize,
    owner: u64,
    vcpu: &crate::vcpu::AxVCpu<A::VCpu>,
) {
    for queued in dispatcher.drain(vcpu_id, owner) {
        match queued.into_virtual() {
            Ok(interrupt) => {
                if let Err(err) = A::inject_vcpu_interrupt(vcpu, interrupt) {
                    warn!(
                        "VM[{vm_id}] VCpu[{vcpu_id}] failed to inject interrupt {interrupt:?}: \
                         {err:?}"
                    );
                }
            }
            Err(interrupt) => A::inject_arch_interrupt(vm_id, vcpu, interrupt),
        }
    }
}

pub(crate) fn target_phys_cpu_ids(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> Vec<usize> {
    let mut cpu_ids = Vec::new();
    for (_, maybe_mask, phys_id) in vcpu_mappings {
        if let Some(mask) = maybe_mask {
            for cpu_id in 0..usize::BITS as usize {
                if mask & (1usize << cpu_id) != 0 && !cpu_ids.contains(&cpu_id) {
                    cpu_ids.push(cpu_id);
                }
            }
        } else if !cpu_ids.contains(phys_id) {
            cpu_ids.push(*phys_id);
        }
    }
    cpu_ids
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use std::{cell::Cell, sync::Arc, vec};

    use ax_std::os::arceos::sync::IrqSafeMutex;
    use axvm_types::{
        GuestPhysAddr, InterruptTriggerMode, NestedPagingConfig, VCpuId, VMId, VmArchPerCpuOps,
        VmArchVcpuOps, VmBackendError, VmBackendResult,
    };

    use super::*;
    use crate::{irq::model::VirtualInterruptId, vcpu::AxVCpu};

    #[derive(Default)]
    struct InjectionLog {
        attempts: Vec<(usize, InterruptTriggerMode)>,
        failing_vector: Option<usize>,
    }

    struct RecordingVcpu {
        injections: Arc<IrqSafeMutex<InjectionLog>>,
    }

    impl VmArchVcpuOps for RecordingVcpu {
        type CreateConfig = Arc<IrqSafeMutex<InjectionLog>>;
        type SetupConfig = ();
        type Exit = ();

        fn new(
            _vm_id: VMId,
            _vcpu_id: VCpuId,
            injections: Self::CreateConfig,
        ) -> VmBackendResult<Self> {
            Ok(Self { injections })
        }

        fn set_entry(&mut self, _entry: GuestPhysAddr) -> VmBackendResult {
            Ok(())
        }

        fn set_nested_page_table(&mut self, _config: NestedPagingConfig) -> VmBackendResult {
            Ok(())
        }

        fn setup(&mut self, _config: Self::SetupConfig) -> VmBackendResult {
            Ok(())
        }

        fn run(&mut self) -> VmBackendResult<Self::Exit> {
            Ok(())
        }

        fn bind(&mut self) -> VmBackendResult {
            Ok(())
        }

        fn unbind(&mut self) -> VmBackendResult {
            Ok(())
        }

        fn set_gpr(&mut self, _reg: usize, _val: usize) {}

        fn inject_interrupt(&mut self, vector: usize) -> VmBackendResult {
            self.record_injection(vector, InterruptTriggerMode::EdgeTriggered)
        }

        fn inject_interrupt_with_trigger(
            &mut self,
            vector: usize,
            trigger: InterruptTriggerMode,
        ) -> VmBackendResult {
            self.record_injection(vector, trigger)
        }

        fn set_return_value(&mut self, _val: usize) {}
    }

    impl RecordingVcpu {
        fn record_injection(
            &self,
            vector: usize,
            trigger: InterruptTriggerMode,
        ) -> VmBackendResult {
            let mut injections = self.injections.lock();
            injections.attempts.push((vector, trigger));
            if injections.failing_vector == Some(vector) {
                Err(VmBackendError::ResourceBusy)
            } else {
                Ok(())
            }
        }
    }

    struct RecordingPerCpu;

    impl VmArchPerCpuOps for RecordingPerCpu {
        fn new(_cpu_id: usize) -> VmBackendResult<Self> {
            Ok(Self)
        }

        fn is_enabled(&self) -> bool {
            true
        }

        fn hardware_enable(&mut self) -> VmBackendResult {
            Ok(())
        }

        fn hardware_disable(&mut self) -> VmBackendResult {
            Ok(())
        }
    }

    struct RecordingArch;

    impl ArchOps for RecordingArch {
        type VCpu = RecordingVcpu;
        type PerCpu = RecordingPerCpu;
        type DeferredRunWork = ();
        type NestedPageTable = crate::arch::current::ArchNestedPageTable;

        fn has_hardware_support() -> bool {
            true
        }

        fn handle_vcpu_exit_unbound(
            _vm: &crate::AxVMRef,
            _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
            _exit: <Self::VCpu as VmArchVcpuOps>::Exit,
        ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>> {
            unreachable!("the injection test never runs a vCPU")
        }

        fn finish_deferred_run_work(
            _vm: &crate::AxVMRef,
            _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
            _work: Self::DeferredRunWork,
        ) -> AxVmResult<VcpuRunAction> {
            unreachable!("the injection test has no deferred work")
        }
    }

    #[test]
    fn run_slice_preparation_occurs_once_across_continued_exits() {
        let preparations = Cell::new(0);
        let entries = Cell::new(0);

        let exit = run_vcpu_slice(
            || {
                preparations.set(preparations.get() + 1);
                Ok(())
            },
            || {
                entries.set(entries.get() + 1);
                Ok(entries.get())
            },
            |entry| {
                Ok(if entry < 3 {
                    BoundVcpuExit::Continue
                } else {
                    BoundVcpuExit::Defer(())
                })
            },
        )
        .unwrap();

        assert!(matches!(exit, BoundVcpuExit::Defer(())));
        assert_eq!(entries.get(), 3);
        assert_eq!(preparations.get(), 1);
    }

    #[test]
    fn exit_handling_runs_after_the_cpu_bound_entry_scope() {
        let cpu_bound = Cell::new(false);
        let handled = Cell::new(false);

        let exit = run_vcpu_slice(
            || Ok(()),
            || {
                assert!(!cpu_bound.replace(true));
                cpu_bound.set(false);
                Ok(())
            },
            |()| {
                assert!(!cpu_bound.get());
                handled.set(true);
                Ok(BoundVcpuExit::Defer(()))
            },
        )
        .unwrap();

        assert!(matches!(exit, BoundVcpuExit::Defer(())));
        assert!(handled.get());
    }

    #[test]
    fn inject_vcpu_interrupt_preserves_level_trigger_at_backend_boundary() {
        let injections = Arc::new(IrqSafeMutex::new(InjectionLog::default()));
        let vcpu = AxVCpu::<RecordingVcpu>::new(1, 0, None, injections.clone()).unwrap();
        let interrupt = PendingVcpuInterrupt {
            id: VirtualInterruptId(0x31),
            trigger: InterruptTriggerMode::LevelTriggered,
        };
        let dispatcher = crate::runtime::VcpuIrqDispatcher::new();
        dispatcher.register(0, 1);
        dispatcher.enqueue(0, 1, interrupt);

        inject_drained_interrupts::<RecordingArch>(&dispatcher, 1, 0, 1, &vcpu);

        assert_eq!(
            injections.lock().attempts,
            vec![(0x31, InterruptTriggerMode::LevelTriggered)]
        );
    }

    #[test]
    fn dispatcher_drain_injects_fifo_once_and_consumes_failed_entries() {
        let injections = Arc::new(IrqSafeMutex::new(InjectionLog {
            failing_vector: Some(0x42),
            ..Default::default()
        }));
        let vcpu = AxVCpu::<RecordingVcpu>::new(1, 0, None, injections.clone()).unwrap();
        let dispatcher = crate::runtime::VcpuIrqDispatcher::new();
        dispatcher.register(0, 1);
        for interrupt in [
            PendingVcpuInterrupt {
                id: VirtualInterruptId(0x41),
                trigger: InterruptTriggerMode::EdgeTriggered,
            },
            PendingVcpuInterrupt {
                id: VirtualInterruptId(0x42),
                trigger: InterruptTriggerMode::LevelTriggered,
            },
            PendingVcpuInterrupt {
                id: VirtualInterruptId(0x43),
                trigger: InterruptTriggerMode::EdgeTriggered,
            },
        ] {
            dispatcher.enqueue(0, 1, interrupt);
        }

        inject_drained_interrupts::<RecordingArch>(&dispatcher, 1, 0, 1, &vcpu);
        inject_drained_interrupts::<RecordingArch>(&dispatcher, 1, 0, 1, &vcpu);

        assert_eq!(
            injections.lock().attempts,
            vec![
                (0x41, InterruptTriggerMode::EdgeTriggered),
                (0x42, InterruptTriggerMode::LevelTriggered),
                (0x43, InterruptTriggerMode::EdgeTriggered),
            ]
        );
        assert!(dispatcher.drain(0, 1).is_empty());
    }
}

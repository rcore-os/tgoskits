//! Core vCPU and nested-paging contract implemented by every target architecture.

use std::{format, vec::Vec};

use axaddrspace::NestedPageTableOps;
use axvm_types::{VmArchPerCpuOps, VmArchVcpuOps, VmVcpuState};

use super::{BoundVcpuExit, VcpuRunAction};
use crate::{AxVmError, AxVmResult, ax_err, irq::model::PendingVcpuInterrupt};

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

    fn before_vcpu_run(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        Ok(())
    }

    fn wait_for_vcpu_event(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        runtime: &crate::vm::VmRuntimeHandle,
    ) {
        let wait_snapshot = runtime.vcpu_event_wait_snapshot();
        if wait_snapshot.has_pending_vcpu_event(runtime, vcpu.id()) {
            return;
        }
        crate::vm::wait_for_vcpu_event_if_idle(
            runtime,
            &wait_snapshot,
            || vm.running(),
            |condition| {
                runtime.wait_vcpu_until(vcpu.id(), || {
                    condition() || runtime.irq_dispatcher().has_pending(vcpu.id())
                })
            },
        );
    }

    fn inject_pending_interrupt(
        _vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        interrupt: crate::vm::PendingInterrupt,
    ) {
        match interrupt {
            crate::vm::PendingInterrupt::Normal(vector) => {
                trace!(
                    "Injecting queued interrupt {vector:#x} into VM[{}] VCpu[{}]",
                    vcpu.vm_id(),
                    vcpu.id()
                );
                if let Err(err) = vcpu.inject_interrupt(vector) {
                    warn!(
                        "Failed to inject queued interrupt {vector:#x} into VM[{}] VCpu[{}]: \
                         {err:?}",
                        vcpu.vm_id(),
                        vcpu.id()
                    );
                }
            }
            crate::vm::PendingInterrupt::External {
                vector,
                physical_irq,
            } => {
                warn!(
                    "VM[{}] VCpu[{}] dropped unsupported external interrupt vector={vector:#x}, \
                     physical_irq={physical_irq:#x}",
                    vcpu.vm_id(),
                    vcpu.id()
                );
            }
        }
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

    /// Returns true when the backend cannot currently accept another edge for
    /// `vector` (for example the GIC list register for it is already pending
    /// or active). Drain loops re-queue such interrupts instead of dropping
    /// them.
    fn is_virtual_interrupt_busy(_vector: usize) -> bool {
        false
    }

    /// Releases architecture runtime state after the VM's last vCPU exits.
    ///
    /// The VM reference is required for architecture state that is published
    /// through VM-local device services rather than indexed in global tables.
    fn on_last_vcpu_exit(vm: &crate::AxVMRef) -> AxVmResult {
        Self::exit_runtime(vm)
    }

    fn handle_vcpu_exit_bound(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>>;

    fn finish_deferred_run_work(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxVmResult<VcpuRunAction>;
}

pub(super) fn run_vcpu<A: super::Architecture>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
) -> AxVmResult<VcpuRunAction> {
    let vm_id = vm.id();
    let vcpu_id = vcpu.id();

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

    let run_result = vcpu.with_current_cpu_set(|| -> AxVmResult<_> {
        loop {
            crate::runtime::vcpus::inject_pending_interrupts::<A>(vm.id(), vcpu_id, vcpu);

            drain_and_inject_dispatched_interrupts::<A>(vm, vcpu_id, vcpu);

            A::before_vcpu_run(vm, vcpu)?;
            let exit = vcpu.run();
            let exit = exit?;
            trace!("{exit:#x?}");
            match A::handle_vcpu_exit_bound(vm, vcpu, exit)? {
                BoundVcpuExit::Continue => continue,
                action => break Ok(action),
            }
        }
    });

    let unbind_result = vcpu.unbind();
    match run_result {
        Ok(BoundVcpuExit::Complete(action)) => {
            unbind_result?;
            Ok(action)
        }
        Ok(BoundVcpuExit::Defer(work)) => {
            unbind_result?;
            A::finish_deferred_run_work(vm, vcpu, work)
        }
        Ok(BoundVcpuExit::Continue) => unreachable!("continued exits do not leave run loop"),
        Err(err) => {
            if let Err(unbind_err) = unbind_result {
                warn!("VM[{vm_id}] VCpu[{vcpu_id}] unbind after run error failed: {unbind_err:?}");
            }
            Err(err)
        }
    }
}

fn drain_and_inject_dispatched_interrupts<A: ArchOps>(
    vm: &crate::AxVMRef,
    vcpu_id: usize,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
) {
    let runtime = match vm.runtime_handle() {
        Ok(runtime) => runtime,
        Err(err) => {
            warn!(
                "VM[{}] VCpu[{}] cannot access interrupt dispatcher: {:?}",
                vm.id(),
                vcpu_id,
                err
            );
            return;
        }
    };
    runtime.trace_virq_event(vm.id(), crate::runtime::VirqTraceKind::Running, vcpu_id, 0);
    pop_and_inject(
        runtime.irq_dispatcher(),
        vcpu_id,
        |vector| A::is_virtual_interrupt_busy(vector),
        |interrupt| {
            runtime.trace_virq_event(
                vm.id(),
                crate::runtime::VirqTraceKind::Inject,
                vcpu_id,
                interrupt.id.0,
            );
            A::inject_vcpu_interrupt(vcpu, interrupt)
        },
    );
}

/// Pops one injectable edge at a time under the queue lock and injects it.
///
/// A blocked head edge stays queued, so same-vector batches cannot be drained
/// and then dropped on a busy list register. When `inject` reports a retryable
/// failure (for example the backend ran out of list registers between the busy
/// check and the write), the edge is re-queued and the loop stops for this
/// round instead of being lost.
fn pop_and_inject<F, G>(
    dispatcher: &crate::runtime::VcpuIrqDispatcher,
    vcpu_id: usize,
    is_busy: F,
    mut inject: G,
) where
    F: Fn(usize) -> bool,
    G: FnMut(PendingVcpuInterrupt) -> AxVmResult,
{
    while let Some(interrupt) =
        dispatcher.pop_if(vcpu_id, |interrupt| is_busy(interrupt.id.0 as usize))
    {
        let vector = interrupt.id.0 as usize;
        match inject(interrupt) {
            Ok(()) => {}
            Err(error) if is_retryable_injection_error(&error) => {
                if !dispatcher.requeue_retry(vcpu_id, interrupt) {
                    warn!(
                        "vCPU {vcpu_id} retry slot already occupied; dropping retry edge \
                         vector={vector:#x}"
                    );
                }
                break;
            }
            Err(error) => {
                warn!(
                    "vCPU {vcpu_id} dropped interrupt after terminal injection failure \
                     vector={vector:#x}: {error:?}"
                );
            }
        }
    }
}

fn is_retryable_injection_error(error: &AxVmError) -> bool {
    matches!(
        error,
        AxVmError::ResourceConflict {
            resource: "interrupt backend",
            ..
        }
    )
}

#[cfg(test)]
fn inject_drained_interrupts<A: ArchOps>(
    dispatcher: &crate::runtime::VcpuIrqDispatcher,
    vm_id: usize,
    vcpu_id: usize,
    vcpu: &crate::vcpu::AxVCpu<A::VCpu>,
) {
    for interrupt in dispatcher.drain(vcpu_id) {
        if let Err(err) = A::inject_vcpu_interrupt(vcpu, interrupt) {
            warn!("VM[{vm_id}] VCpu[{vcpu_id}] failed to inject interrupt {interrupt:?}: {err:?}");
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
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        vec,
    };

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

    static EXIT_RUNTIME_CALLS: AtomicUsize = AtomicUsize::new(0);

    impl ArchOps for RecordingArch {
        type VCpu = RecordingVcpu;
        type PerCpu = RecordingPerCpu;
        type DeferredRunWork = ();
        type NestedPageTable = crate::arch::current::ArchNestedPageTable;

        fn has_hardware_support() -> bool {
            true
        }

        fn exit_runtime(_vm: &crate::AxVM) -> AxVmResult {
            EXIT_RUNTIME_CALLS.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn handle_vcpu_exit_bound(
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
    fn inject_vcpu_interrupt_preserves_level_trigger_at_backend_boundary() {
        let injections = Arc::new(IrqSafeMutex::new(InjectionLog::default()));
        let vcpu = AxVCpu::<RecordingVcpu>::new(1, 0, None, injections.clone()).unwrap();
        let interrupt = PendingVcpuInterrupt {
            id: VirtualInterruptId(0x31),
            trigger: InterruptTriggerMode::LevelTriggered,
        };
        let dispatcher = crate::runtime::VcpuIrqDispatcher::new();
        dispatcher.register_test_vcpu(0, 2);
        dispatcher.enqueue(0, interrupt).unwrap();

        inject_drained_interrupts::<RecordingArch>(&dispatcher, 1, 0, &vcpu);

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
        dispatcher.register_test_vcpu(0, 2);
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
            dispatcher.enqueue(0, interrupt).unwrap();
        }

        inject_drained_interrupts::<RecordingArch>(&dispatcher, 1, 0, &vcpu);
        inject_drained_interrupts::<RecordingArch>(&dispatcher, 1, 0, &vcpu);

        assert_eq!(
            injections.lock().attempts,
            vec![
                (0x41, InterruptTriggerMode::EdgeTriggered),
                (0x42, InterruptTriggerMode::LevelTriggered),
                (0x43, InterruptTriggerMode::EdgeTriggered),
            ]
        );
        assert!(dispatcher.drain(0).is_empty());
    }

    #[test]
    fn last_vcpu_exit_uses_common_runtime_exit_by_default() {
        EXIT_RUNTIME_CALLS.store(0, Ordering::Relaxed);
        let vm = crate::vm::destroyed_vm_for_test(73);

        RecordingArch::on_last_vcpu_exit(&vm).unwrap();

        assert_eq!(EXIT_RUNTIME_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn pop_and_inject_requeues_edge_on_retryable_backend_failure() {
        let injections = Arc::new(IrqSafeMutex::new(InjectionLog {
            failing_vector: Some(0x42),
            ..Default::default()
        }));
        let vcpu = AxVCpu::<RecordingVcpu>::new(1, 0, None, injections.clone()).unwrap();
        let dispatcher = crate::runtime::VcpuIrqDispatcher::new();
        dispatcher.register_test_vcpu(0, 2);
        for id in [0x41u32, 0x42, 0x43] {
            dispatcher
                .enqueue(
                    0,
                    PendingVcpuInterrupt {
                        id: VirtualInterruptId(id),
                        trigger: InterruptTriggerMode::EdgeTriggered,
                    },
                )
                .unwrap();
        }

        pop_and_inject(
            &dispatcher,
            0,
            |_| false,
            |interrupt| {
                vcpu.inject_interrupt_with_trigger(interrupt.id.0 as usize, interrupt.trigger)
            },
        );

        // 0x41 injected; 0x42 was attempted and failed (recorded, then kept in
        // the retry slot rather than dropped); the loop stops before 0x43.
        assert_eq!(
            injections.lock().attempts,
            vec![
                (0x41, InterruptTriggerMode::EdgeTriggered),
                (0x42, InterruptTriggerMode::EdgeTriggered),
            ]
        );
        let remaining = dispatcher.drain(0);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id.0, 0x43);
        // The failed edge survives in the retry slot.
        let retried = dispatcher.pop_if(0, |_| false).unwrap();
        assert_eq!(retried.id.0, 0x42);
    }

    #[test]
    fn retry_slot_holds_edge_outside_bounded_queue() {
        let dispatcher = crate::runtime::VcpuIrqDispatcher::new();
        dispatcher.register_test_vcpu(0, 2);
        for id in 0..crate::runtime::VCPU_INTERRUPT_QUEUE_CAPACITY as u32 {
            dispatcher
                .enqueue(
                    0,
                    PendingVcpuInterrupt {
                        id: VirtualInterruptId(id),
                        trigger: InterruptTriggerMode::EdgeTriggered,
                    },
                )
                .unwrap();
        }

        assert!(dispatcher.requeue_retry(
            0,
            PendingVcpuInterrupt {
                id: VirtualInterruptId(999),
                trigger: InterruptTriggerMode::EdgeTriggered,
            }
        ));

        // The retry edge is served first and does not depend on queue
        // capacity, while the full queue still rejects new producers.
        assert_eq!(dispatcher.pop_if(0, |_| false).unwrap().id.0, 999);
        assert!(
            dispatcher
                .enqueue(
                    0,
                    PendingVcpuInterrupt {
                        id: VirtualInterruptId(1000),
                        trigger: InterruptTriggerMode::EdgeTriggered,
                    }
                )
                .is_err()
        );
    }

    #[test]
    fn retry_slot_keeps_blocked_edge() {
        let dispatcher = crate::runtime::VcpuIrqDispatcher::new();
        dispatcher.register_test_vcpu(0, 2);
        dispatcher.requeue_retry(
            0,
            PendingVcpuInterrupt {
                id: VirtualInterruptId(7),
                trigger: InterruptTriggerMode::EdgeTriggered,
            },
        );

        assert!(
            dispatcher
                .pop_if(0, |interrupt| interrupt.id.0 == 7)
                .is_none()
        );
        assert_eq!(dispatcher.pop_if(0, |_| false).unwrap().id.0, 7);
    }

    #[test]
    fn retry_slot_counts_as_pending_for_wait_condition() {
        let dispatcher = crate::runtime::VcpuIrqDispatcher::new();
        dispatcher.register_test_vcpu(0, 2);
        assert!(!dispatcher.has_pending(0));

        dispatcher.requeue_retry(
            0,
            PendingVcpuInterrupt {
                id: VirtualInterruptId(7),
                trigger: InterruptTriggerMode::EdgeTriggered,
            },
        );

        // A retry edge is pending work: the vCPU must not park until it is
        // delivered, otherwise the edge is stranded when producers stop.
        assert!(dispatcher.has_pending(0));

        assert!(dispatcher.pop_if(0, |_| false).is_some());
        assert!(!dispatcher.has_pending(0));
    }

    #[test]
    fn retained_blocked_edge_is_retried_after_backend_releases() {
        let injections = Arc::new(IrqSafeMutex::new(InjectionLog::default()));
        let vcpu = AxVCpu::<RecordingVcpu>::new(1, 0, None, injections.clone()).unwrap();
        let dispatcher = crate::runtime::VcpuIrqDispatcher::new();
        dispatcher.register_test_vcpu(0, 2);
        dispatcher
            .enqueue(
                0,
                PendingVcpuInterrupt {
                    id: VirtualInterruptId(0x51),
                    trigger: InterruptTriggerMode::EdgeTriggered,
                },
            )
            .unwrap();

        let backend_busy = Arc::new(IrqSafeMutex::new(true));

        // Round 1: the last enqueued edge finds the LR busy and stays queued;
        // has_pending stays true so the vCPU cannot park.
        let busy = Arc::clone(&backend_busy);
        pop_and_inject(
            &dispatcher,
            0,
            move |vector| *busy.lock() && vector == 0x51,
            |interrupt| {
                vcpu.inject_interrupt_with_trigger(interrupt.id.0 as usize, interrupt.trigger)
            },
        );
        assert!(injections.lock().attempts.is_empty());
        assert!(dispatcher.has_pending(0));

        // Round 2: the LR is released; the retained edge is drained and
        // injected instead of being stranded.
        *backend_busy.lock() = false;
        pop_and_inject(
            &dispatcher,
            0,
            |_| false,
            |interrupt| {
                vcpu.inject_interrupt_with_trigger(interrupt.id.0 as usize, interrupt.trigger)
            },
        );
        assert_eq!(
            injections.lock().attempts,
            vec![(0x51, InterruptTriggerMode::EdgeTriggered)]
        );
        assert!(!dispatcher.has_pending(0));
    }

    #[test]
    fn terminal_backend_failure_does_not_keep_vcpu_pending() {
        let dispatcher = crate::runtime::VcpuIrqDispatcher::new();
        dispatcher.register_test_vcpu(0, 2);
        dispatcher
            .enqueue(
                0,
                PendingVcpuInterrupt {
                    id: VirtualInterruptId(0x61),
                    trigger: InterruptTriggerMode::EdgeTriggered,
                },
            )
            .unwrap();

        pop_and_inject(
            &dispatcher,
            0,
            |_| false,
            |_| {
                Err(AxVmError::invalid_input(
                    "inject vCPU interrupt",
                    "invalid vector",
                ))
            },
        );

        assert!(!dispatcher.has_pending(0));
    }
}

//! Core vCPU and nested-paging contract implemented by every target architecture.

use std::{format, vec::Vec};

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

    fn before_vcpu_run(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        Ok(())
    }

    fn wait_for_vcpu_event(
        vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        runtime: &crate::vm::VmRuntimeHandle,
    ) {
        let wait_snapshot = runtime.vcpu_event_wait_snapshot();
        crate::vm::wait_for_vcpu_event_if_idle(
            runtime,
            &wait_snapshot,
            || vm.running(),
            |condition| runtime.wait_until(condition),
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
    let runtime = match vm.with_runtime(|runtime| Ok(runtime.clone())) {
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
    inject_drained_interrupts::<A>(runtime.irq_dispatcher(), vm.id(), vcpu_id, vcpu);
}

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
}

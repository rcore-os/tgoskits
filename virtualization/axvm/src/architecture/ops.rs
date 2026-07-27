//! Core vCPU and nested-paging contract implemented by every target architecture.

use alloc::{format, vec::Vec};

use ax_memory_addr::VirtAddr;
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

    fn clean_dcache_range(_addr: VirtAddr, _size: usize) {}

    fn register_platform_irq_injector() {}

    fn vcpu_affinities(
        cpu_num: usize,
        phys_cpu_ids: Option<&[usize]>,
        phys_cpu_sets: Option<&[usize]>,
    ) -> Vec<(usize, Option<usize>, usize)> {
        default_vcpu_affinities(cpu_num, phys_cpu_ids, phys_cpu_sets)
    }

    fn set_vcpu_on_args(vcpu: &crate::vm::AxVCpuRef<Self::VCpu>, _vcpu_id: usize, arg: usize) {
        vcpu.set_gpr(0, arg);
    }

    fn before_first_run(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {}

    fn before_vcpu_run(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {}

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
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        interrupt: PendingVcpuInterrupt,
    ) -> AxVmResult {
        vcpu.inject_interrupt_with_trigger(interrupt.id.0 as usize, interrupt.trigger)
    }

    fn after_external_interrupt(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        vector: usize,
    ) {
        crate::host::arceos::dispatch_host_irq(vector);
        crate::check_timer_events();
    }

    fn on_last_vcpu_exit(_vm_id: usize) {}

    fn after_mmio_write(_vm: &crate::AxVMRef) {}

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

    fn run_vcpu(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult<VcpuRunAction>
    where
        Self: Sized,
    {
        let vm_id = vm.id();
        let vcpu_id = vcpu.id();

        match vcpu.state() {
            VmVcpuState::Free => vcpu.bind()?,
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
                crate::runtime::vcpus::inject_pending_interrupts::<Self>(vm.id(), vcpu_id, vcpu);

                drain_and_inject_dispatched_interrupts::<Self>(vm, vcpu_id, vcpu);

                let exit = vcpu.run()?;
                trace!("{exit:#x?}");
                match Self::handle_vcpu_exit_bound(vm, vcpu, exit)? {
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
                Self::finish_deferred_run_work(vm, vcpu, work)
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
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
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

pub(crate) fn default_vcpu_affinities(
    cpu_num: usize,
    phys_cpu_ids: Option<&[usize]>,
    phys_cpu_sets: Option<&[usize]>,
) -> Vec<(usize, Option<usize>, usize)> {
    let mut vcpus = Vec::with_capacity(cpu_num);
    for vcpu_id in 0..cpu_num {
        vcpus.push((vcpu_id, None, vcpu_id));
    }

    if let Some(phys_cpu_sets) = phys_cpu_sets {
        for (vcpu_id, pcpu_mask_bitmap) in phys_cpu_sets.iter().enumerate() {
            if let Some(vcpu) = vcpus.get_mut(vcpu_id) {
                vcpu.1 = Some(*pcpu_mask_bitmap);
            }
        }
    }

    if let Some(phys_cpu_ids) = phys_cpu_ids {
        for (vcpu_id, phys_id) in phys_cpu_ids.iter().enumerate() {
            if let Some(vcpu) = vcpus.get_mut(vcpu_id) {
                vcpu.2 = *phys_id;
            }
        }
    }

    vcpus
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use alloc::{sync::Arc, vec};

    use ax_kspin::SpinNoIrq;
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
        injections: Arc<SpinNoIrq<InjectionLog>>,
    }

    impl VmArchVcpuOps for RecordingVcpu {
        type CreateConfig = Arc<SpinNoIrq<InjectionLog>>;
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
        type NestedPageTable = crate::arch::ArchNestedPageTable;

        fn has_hardware_support() -> bool {
            true
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
        let injections = Arc::new(SpinNoIrq::new(InjectionLog::default()));
        let vcpu = Arc::new(AxVCpu::<RecordingVcpu>::new(1, 0, None, injections.clone()).unwrap());
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
        let injections = Arc::new(SpinNoIrq::new(InjectionLog {
            failing_vector: Some(0x42),
            ..Default::default()
        }));
        let vcpu = Arc::new(AxVCpu::<RecordingVcpu>::new(1, 0, None, injections.clone()).unwrap());
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
}

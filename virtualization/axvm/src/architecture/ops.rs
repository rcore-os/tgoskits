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

    /// Releases architecture runtime state after the VM's last vCPU exits.
    ///
    /// The VM reference is required for architecture state that is published
    /// through VM-local device services rather than indexed in global tables.
    fn on_last_vcpu_exit(_vm: &crate::AxVMRef) {}

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
        let bound_exit = with_bound_vcpu(vm_id, vcpu_id, vcpu, || {
            run_bound_vcpu::<Self>(vm, vcpu_id, vcpu)
        })?;

        match bound_exit {
            BoundVcpuExit::Complete(action) => Ok(action),
            BoundVcpuExit::Defer(work) => Self::finish_deferred_run_work(vm, vcpu, work),
            BoundVcpuExit::Continue => unreachable!("continued exits do not leave run loop"),
        }
    }
}

fn with_bound_vcpu<A: VmArchVcpuOps, T>(
    vm_id: usize,
    vcpu_id: usize,
    vcpu: &crate::vm::AxVCpuRef<A>,
    operation: impl FnOnce() -> AxVmResult<T>,
) -> AxVmResult<T> {
    vcpu.with_current_cpu_set(|cpu_pin| {
        ensure_vcpu_bound(vcpu, cpu_pin)?;
        let run_result = operation();
        let unbind_result = vcpu.unbind_after_run(cpu_pin);
        finish_bound_run(vm_id, vcpu_id, run_result, unbind_result)
    })
}

fn ensure_vcpu_bound<A: VmArchVcpuOps>(
    vcpu: &crate::vm::AxVCpuRef<A>,
    cpu_pin: &ax_percpu::CpuPin<'_>,
) -> AxVmResult {
    ensure_vcpu_is_free(vcpu)?;
    vcpu.bind(cpu_pin)
}

fn ensure_vcpu_is_free<A: VmArchVcpuOps>(vcpu: &crate::vm::AxVCpuRef<A>) -> AxVmResult {
    let state = vcpu.state();
    if state == VmVcpuState::Free {
        Ok(())
    } else {
        ax_err!(BadState, format!("VCpu state is not Free, but {state:?}"))
    }
}

fn run_bound_vcpu<A: ArchOps>(
    vm: &crate::AxVMRef,
    vcpu_id: usize,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
) -> AxVmResult<BoundVcpuExit<A::DeferredRunWork>> {
    loop {
        crate::runtime::vcpus::inject_pending_interrupts::<A>(vm.id(), vcpu_id, vcpu);
        drain_and_inject_dispatched_interrupts::<A>(vm, vcpu_id, vcpu);

        let exit = vcpu.run()?;
        trace!("{exit:#x?}");
        match A::handle_vcpu_exit_bound(vm, vcpu, exit)? {
            BoundVcpuExit::Continue => continue,
            action => return Ok(action),
        }
    }
}

fn finish_bound_run<T>(
    vm_id: usize,
    vcpu_id: usize,
    run_result: AxVmResult<T>,
    unbind_result: AxVmResult,
) -> AxVmResult<T> {
    match (run_result, unbind_result) {
        (Ok(result), Ok(())) => Ok(result),
        (Ok(_), Err(unbind_error)) => Err(unbind_error),
        (Err(run_error), Ok(())) => Err(run_error),
        (Err(run_error), Err(unbind_error)) => {
            warn!("VM[{vm_id}] VCpu[{vcpu_id}] unbind after run error failed: {unbind_error:?}");
            Err(run_error)
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
        GuestPhysAddr, HostPhysAddr, InterruptTriggerMode, NestedPagingConfig, VCpuId, VMId,
        VmArchPerCpuOps, VmArchVcpuOps, VmBackendError, VmBackendResult,
    };

    use super::*;
    use crate::{AxVmError, irq::model::VirtualInterruptId, vcpu::AxVCpu};

    #[derive(Default)]
    struct InjectionLog {
        attempts: Vec<(usize, InterruptTriggerMode)>,
        failing_vector: Option<usize>,
        lifecycle: Vec<LifecycleEvent>,
        bind_error: Option<VmBackendError>,
        run_error: Option<VmBackendError>,
        unbind_error: Option<VmBackendError>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum LifecycleStep {
        Bind,
        Run,
        ExitHandler,
        Unbind,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct LifecycleEvent {
        step: LifecycleStep,
        cpu_id: usize,
        current_vcpu_published: bool,
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
            self.record_lifecycle(LifecycleStep::Run);
            self.injections.lock().run_error.map_or(Ok(()), Err)
        }

        fn bind(&mut self) -> VmBackendResult {
            self.record_lifecycle(LifecycleStep::Bind);
            self.injections.lock().bind_error.map_or(Ok(()), Err)
        }

        fn unbind(&mut self) -> VmBackendResult {
            self.record_lifecycle(LifecycleStep::Unbind);
            self.injections.lock().unbind_error.map_or(Ok(()), Err)
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
        fn record_lifecycle(&self, step: LifecycleStep) {
            self.injections.lock().lifecycle.push(lifecycle_event(step));
        }

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

    #[test]
    fn cleanup_error_is_propagated_after_a_successful_bound_run() {
        let cleanup_error = AxVmError::invalid_state("unbind vCPU", VmBackendError::InvalidState);

        assert!(matches!(
            finish_bound_run(3, 2, Ok(7), Err(cleanup_error)),
            Err(AxVmError::InvalidState {
                operation: "unbind vCPU",
                ..
            })
        ));
    }

    #[test]
    fn primary_run_error_wins_when_cleanup_also_fails() {
        let run_error = AxVmError::vcpu("run vCPU", VmBackendError::InvalidData);
        let cleanup_error = AxVmError::invalid_state("unbind vCPU", VmBackendError::InvalidState);

        assert!(matches!(
            finish_bound_run::<()>(3, 2, Err(run_error), Err(cleanup_error)),
            Err(AxVmError::Vcpu {
                operation: "run vCPU",
                ..
            })
        ));
    }

    #[test]
    fn preexisting_ready_state_cannot_bypass_run_path_binding() {
        let injections = Arc::new(SpinNoIrq::new(InjectionLog::default()));
        let vcpu = Arc::new(AxVCpu::<RecordingVcpu>::new(1, 0, None, injections).unwrap());
        vcpu.transition_state(VmVcpuState::Created, VmVcpuState::Ready)
            .unwrap();

        assert!(ensure_vcpu_is_free(&vcpu).is_err());
    }

    #[test]
    fn bound_lifecycle_keeps_cpu_identity_and_publication_through_unbind() {
        let (vcpu, log) = ready_recording_vcpu(InjectionLog::default());

        with_bound_vcpu(1, 0, &vcpu, || {
            vcpu.run()?;
            log.lock()
                .lifecycle
                .push(lifecycle_event(LifecycleStep::ExitHandler));
            Ok(())
        })
        .unwrap();

        assert_eq!(
            log.lock().lifecycle,
            [
                event_on_cpu_zero(LifecycleStep::Bind),
                event_on_cpu_zero(LifecycleStep::Run),
                event_on_cpu_zero(LifecycleStep::ExitHandler),
                event_on_cpu_zero(LifecycleStep::Unbind),
            ]
        );
        assert!(
            crate::vcpu::with_current_vcpu::<RecordingVcpu, _>(|current| current.is_none()),
            "publication must be withdrawn after successful unbind"
        );
        assert_eq!(vcpu.state(), VmVcpuState::Free);
    }

    #[test]
    fn bind_failure_skips_run_and_unbind() {
        let (vcpu, log) = ready_recording_vcpu(InjectionLog {
            bind_error: Some(VmBackendError::InvalidState),
            ..Default::default()
        });

        assert!(with_bound_vcpu(1, 0, &vcpu, || Ok(())).is_err());
        assert_eq!(
            log.lock().lifecycle,
            [event_on_cpu_zero(LifecycleStep::Bind)]
        );
    }

    #[test]
    fn run_failure_still_unbinds_before_withdrawing_publication() {
        let (vcpu, log) = ready_recording_vcpu(InjectionLog {
            run_error: Some(VmBackendError::InvalidData),
            ..Default::default()
        });

        assert!(with_bound_vcpu(1, 0, &vcpu, || vcpu.run()).is_err());
        assert_eq!(
            log.lock().lifecycle,
            [
                event_on_cpu_zero(LifecycleStep::Bind),
                event_on_cpu_zero(LifecycleStep::Run),
                event_on_cpu_zero(LifecycleStep::Unbind),
            ]
        );
    }

    #[test]
    fn exit_handler_failure_still_unbinds() {
        let (vcpu, log) = ready_recording_vcpu(InjectionLog::default());

        let result = with_bound_vcpu(1, 0, &vcpu, || {
            vcpu.run()?;
            log.lock()
                .lifecycle
                .push(lifecycle_event(LifecycleStep::ExitHandler));
            Err::<(), _>(AxVmError::vcpu(
                "handle vCPU exit",
                VmBackendError::InvalidData,
            ))
        });

        assert!(result.is_err());
        assert_eq!(
            log.lock().lifecycle,
            [
                event_on_cpu_zero(LifecycleStep::Bind),
                event_on_cpu_zero(LifecycleStep::Run),
                event_on_cpu_zero(LifecycleStep::ExitHandler),
                event_on_cpu_zero(LifecycleStep::Unbind),
            ]
        );
    }

    #[test]
    fn unbind_failure_is_propagated_after_successful_run() {
        let (vcpu, log) = ready_recording_vcpu(InjectionLog {
            unbind_error: Some(VmBackendError::InvalidState),
            ..Default::default()
        });

        assert!(with_bound_vcpu(1, 0, &vcpu, || vcpu.run()).is_err());
        assert_eq!(
            log.lock().lifecycle,
            [
                event_on_cpu_zero(LifecycleStep::Bind),
                event_on_cpu_zero(LifecycleStep::Run),
                event_on_cpu_zero(LifecycleStep::Unbind),
            ]
        );
    }

    fn ready_recording_vcpu(
        log: InjectionLog,
    ) -> (Arc<AxVCpu<RecordingVcpu>>, Arc<SpinNoIrq<InjectionLog>>) {
        install_test_cpu_area();
        let log = Arc::new(SpinNoIrq::new(log));
        let vcpu = Arc::new(AxVCpu::<RecordingVcpu>::new(1, 0, None, log.clone()).unwrap());
        vcpu.setup(
            GuestPhysAddr::from_usize(0),
            NestedPagingConfig::new(HostPhysAddr::from_usize(0), 3, 39, 0),
            (),
        )
        .unwrap();
        (vcpu, log)
    }

    fn install_test_cpu_area() {
        use core::alloc::Layout;

        let size = core::mem::size_of::<cpu_local::CpuAreaPrefix>();
        let layout = Layout::from_size_align(size, 4096).unwrap();
        // SAFETY: the zeroed allocation is checked and intentionally leaked
        // for the process lifetime. Host tests use a thread-local current-vCPU
        // publication, so only the fixed CPU-area prefix is required here.
        let area = unsafe {
            let base = std::alloc::alloc_zeroed(layout);
            assert!(!base.is_null());
            base.cast::<cpu_local::CpuAreaPrefix>().write(
                cpu_local::CpuAreaPrefix::initialize(
                    ax_percpu::CpuIndex::try_from(0).unwrap(),
                    base as usize,
                )
                .unwrap(),
            );
            cpu_local::CpuAreaRef::from_initialized_base(base as usize).unwrap()
        };
        // SAFETY: this host test thread models offline logical CPU 0 and the
        // leaked allocation remains valid until process shutdown.
        unsafe { cpu_local::install_cpu_area(area) }.unwrap();
    }

    fn lifecycle_event(step: LifecycleStep) -> LifecycleEvent {
        let _guard = ax_kernel_guard::NoPreempt::new();
        // SAFETY: the guard prevents migration while observing both the CPU
        // identity and the current-vCPU publication.
        unsafe {
            ax_percpu::with_cpu_pin(|pin| LifecycleEvent {
                step,
                cpu_id: pin.area().cpu_index().as_usize(),
                current_vcpu_published: crate::vcpu::get_current_vcpu::<RecordingVcpu>(pin)
                    .is_some(),
            })
        }
        .unwrap()
    }

    const fn event_on_cpu_zero(step: LifecycleStep) -> LifecycleEvent {
        LifecycleEvent {
            step,
            cpu_id: 0,
            current_vcpu_published: true,
        }
    }
}

//! Sleepable services for one immutable, generation-bound execution period.

use std::{
    sync::{
        Arc, Condvar, Mutex, PoisonError, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axdevice::{
    DeviceManagerError, DeviceManagerResult, DeviceRuntime, GuestCpuFreqPort, RuntimeAccessPorts,
    StopAccessPort, TimerAccessPort, WakeAccessPort, WorkAccessPort,
};
use axdevice_base::{DeviceAccess, DeviceId, DeviceVcpuId};

use crate::{
    AxVmError, AxVmResult, RunId, StopReason,
    guest_memory::GuestMemoryPort,
    host::{HostTime, HostTimer, default_host},
    irq::model::SourceEvent,
    manager::{ControlShared, GuestCpuFreqBackend, GuestCpuFreqError},
    sync::MutexExt,
};

mod signals;
pub(crate) use signals::{RunSignalWorker, RunSignals, SignalError, VcpuWait};

/// A device's durable completion is published before this port is notified.
#[derive(Clone)]
pub(crate) struct DeviceWorkPort {
    target: Arc<dyn WorkAccessPort>,
    device: DeviceId,
}

impl DeviceWorkPort {
    pub(crate) fn from_device_signal(target: Arc<dyn WorkAccessPort>, device: DeviceId) -> Self {
        Self { target, device }
    }

    pub(crate) fn notify(&self) -> Result<(), SignalError> {
        self.target
            .notify_work(self.device)
            .map_err(|_| SignalError::Closed)
    }
}

/// A fixed virtual edge source and target bound to one execution period.
/// It contains only lower signal state and may publish from IRQ context.
#[derive(Clone)]
pub struct VcpuInterruptPort {
    signals: Arc<RunSignals>,
    vcpu_id: usize,
    vector: u32,
}

impl VcpuInterruptPort {
    pub(crate) fn new(signals: Arc<RunSignals>, vcpu_id: usize, vector: u32) -> AxVmResult<Self> {
        if !signals.contains_vcpu(vcpu_id) {
            return Err(AxVmError::invalid_input(
                "bind vCPU interrupt",
                "invalid target vCPU",
            ));
        }
        #[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))]
        if vector >= 256 || (cfg!(target_arch = "x86_64") && vector < 32) {
            return Err(AxVmError::invalid_input(
                "bind vCPU interrupt",
                "vector is outside the guest interrupt namespace",
            ));
        }
        Ok(Self {
            signals,
            vcpu_id,
            vector,
        })
    }

    /// Publishes the edge before waking its current owner. An inactive target
    /// retains the source for a later activation; a closed run rejects it.
    pub fn pulse(&self) -> Result<(), SignalError> {
        self.signals.publish(
            self.vcpu_id,
            crate::irq::model::PendingVcpuInterrupt {
                id: crate::irq::model::VirtualInterruptId(self.vector),
                trigger: crate::InterruptTriggerMode::EdgeTriggered,
                source: None,
            },
        )?;
        match self.signals.kick_from_irq(self.vcpu_id) {
            Ok(()) | Err(SignalError::InactiveTarget) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Routes and task capabilities are sealed before any vCPU can enter the guest.
pub(crate) struct RunServices {
    run: RunId,
    devices: Arc<DeviceRuntime>,
    memory: GuestMemoryPort,
    signals: Arc<RunSignals>,
}

impl RunServices {
    pub(crate) fn new(
        run: RunId,
        devices: Arc<DeviceRuntime>,
        memory: GuestMemoryPort,
        signals: Arc<RunSignals>,
    ) -> Self {
        Self {
            run,
            devices,
            memory,
            signals,
        }
    }

    pub(crate) const fn run_id(&self) -> RunId {
        self.run
    }

    pub(crate) fn devices(&self) -> &DeviceRuntime {
        &self.devices
    }

    pub(crate) fn memory(&self) -> GuestMemoryPort {
        self.memory.clone()
    }

    pub(crate) fn signals(&self) -> &Arc<RunSignals> {
        &self.signals
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn active_mask(&self) -> usize {
        self.signals.active_mask()
    }

    pub(crate) fn read_device(&self, access: &DeviceAccess) -> AxVmResult<Option<u64>> {
        self.devices
            .try_read(access)
            .map_err(|error| AxVmError::device("read guest device", error))
    }

    pub(crate) fn write_device(&self, access: &DeviceAccess, value: u64) -> AxVmResult<bool> {
        self.memory
            .with_access(|memory| self.devices.try_write(access, value, Some(memory)))
            .map_err(|error| AxVmError::device("acquire guest device memory", error))?
            .map_err(|error| AxVmError::device("write guest device", error))
    }

    /// Applies one run-bound shared-controller event on the lifecycle owner.
    /// Producers only publish into the fixed ingress; this dispatch point is
    /// reached from the owner task after the vCPU and IRQ layers are quiescent.
    pub(crate) fn submit_interrupt(&self, event: SourceEvent) -> AxVmResult {
        crate::arch::current::apply_interrupt_event(&self.devices, event)
    }

    /// The designated poller always polls before WFI, including local queue work
    /// that has not yet submitted a request to its asynchronous backend.
    pub(crate) fn poll_devices(&self, vcpu_id: usize, before_wait: bool) -> AxVmResult {
        if self.signals.poll_owner() != Some(vcpu_id) {
            return Ok(());
        }
        if !before_wait && !self.signals.take_work(vcpu_id) {
            return Ok(());
        }
        let now = default_host().monotonic_time().as_nanos() as u64;
        for device in self.devices.iter_pollable_dev() {
            device
                .poll(now)
                .map_err(|error| AxVmError::device("poll guest device", error))?;
        }
        let mut failure = None;
        self.memory
            .with_access(|memory| {
                self.devices.poll_dma_devices(now, memory, |result| {
                    if let Err(error) = result {
                        failure.get_or_insert(error);
                    }
                });
            })
            .map_err(|error| AxVmError::device("acquire guest DMA memory", error))?;
        match failure {
            Some(error) => Err(AxVmError::device("poll guest DMA device", error)),
            None => Ok(()),
        }
    }
}

type TimerHandle = <crate::host::arceos::ArceOsHost as HostTimer>::TimerHandle;

struct TimerRecord {
    deadline: Duration,
    handle: Option<TimerHandle>,
    fired: Arc<AtomicBool>,
}

struct TimerState {
    open: bool,
    callbacks: usize,
    publications: usize,
    records: Vec<TimerRecord>,
}

/// Task-owned timer publication and callback retirement. It is never reachable
/// through a hardware entry or an IRQ endpoint.
struct DeviceTimers {
    state: Mutex<TimerState>,
    idle: Condvar,
    signals: Arc<RunSignals>,
}

impl DeviceTimers {
    fn new(signals: Arc<RunSignals>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(TimerState {
                open: true,
                callbacks: 0,
                publications: 0,
                records: Vec::new(),
            }),
            idle: Condvar::new(),
            signals,
        })
    }

    fn schedule(self: &Arc<Self>, deadline: Duration) -> AxVmResult {
        {
            let mut state = self.state.lock_unpoisoned();
            if !state.open {
                return Err(AxVmError::EntryClosed {
                    vm: self.signals.run_id().vm(),
                });
            }
            let callbacks = state.callbacks.checked_add(1).ok_or_else(|| {
                AxVmError::resource_unavailable("device timer", "callback count exhausted")
            })?;
            let publications = state.publications.checked_add(1).ok_or_else(|| {
                AxVmError::resource_unavailable("device timer", "publication count exhausted")
            })?;
            state.callbacks = callbacks;
            state.publications = publications;
        }
        let publication = TimerPublicationLease {
            timers: self.clone(),
        };
        let lease = TimerCallbackLease {
            timers: self.clone(),
        };
        let signals = self.signals.clone();
        let fired = Arc::new(AtomicBool::new(false));
        let callback_fired = fired.clone();
        let result = default_host().register_timer(
            deadline,
            Box::new(move |_| {
                let _lease = lease;
                callback_fired.store(true, Ordering::Release);
                let _result = signals.notify_work();
            }),
        );
        // Closure publication preceded this bookkeeping. Even if it completed
        // immediately, cancelling its handle is defined to report completion.
        {
            let mut state = self.state.lock_unpoisoned();
            state
                .records
                .retain(|record| !record.fired.load(Ordering::Acquire));
            if let Ok(handle) = result {
                state.records.push(TimerRecord {
                    deadline,
                    handle: Some(handle),
                    fired,
                });
            }
        }
        drop(publication);
        result.map(|_| ())
    }

    fn suspend(&self) -> AxVmResult {
        let handles = {
            let mut state = self.state.lock_unpoisoned();
            state.open = false;
            while state.publications != 0 {
                state = self
                    .idle
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            state
                .records
                .iter_mut()
                .enumerate()
                .filter_map(|(index, record)| record.handle.take().map(|handle| (index, handle)))
                .collect::<Vec<_>>()
        };
        let mut failure = None;
        for (index, handle) in handles {
            if let Err(error) = default_host().cancel_timer(handle) {
                failure.get_or_insert(error);
                // A failed cancellation keeps its registration owned for retry.
                self.state.lock_unpoisoned().records[index].handle = Some(handle);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        let mut state = self.state.lock_unpoisoned();
        while state.callbacks != 0 {
            state = self
                .idle
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        Ok(())
    }

    fn resume(self: &Arc<Self>) -> AxVmResult {
        let deadlines = {
            let mut state = self.state.lock_unpoisoned();
            if state.callbacks != 0 || state.publications != 0 {
                return Err(AxVmError::invalid_state(
                    "resume device timers",
                    "timer callbacks or publications are not quiet",
                ));
            }
            let deadlines = state
                .records
                .iter()
                .filter(|record| record.handle.is_none() && !record.fired.load(Ordering::Acquire))
                .map(|record| (record.deadline, record.fired.clone()))
                .collect::<Vec<_>>();
            state.open = true;
            deadlines
        };
        for (deadline, retired) in deadlines {
            match self.schedule(deadline) {
                Ok(()) => retired.store(true, Ordering::Release),
                Err(error) => {
                    // Original pending records remain owned until each
                    // replacement registration succeeded. A failed registration
                    // therefore cannot lose this or later deadlines.
                    return match self.suspend() {
                        Ok(()) => Err(error),
                        Err(rollback) => Err(AxVmError::lifecycle_rollback(
                            "resume device timers",
                            error,
                            rollback,
                        )),
                    };
                }
            }
        }
        Ok(())
    }
}

struct TimerPublicationLease {
    timers: Arc<DeviceTimers>,
}
impl Drop for TimerPublicationLease {
    fn drop(&mut self) {
        {
            let mut state = self.timers.state.lock_unpoisoned();
            state.publications -= 1;
        }
        self.timers.idle.notify_all();
    }
}

struct TimerCallbackLease {
    timers: Arc<DeviceTimers>,
}

impl Drop for TimerCallbackLease {
    fn drop(&mut self) {
        let idle = {
            let mut state = self.timers.state.lock_unpoisoned();
            state.callbacks -= 1;
            state.callbacks == 0
        };
        if idle {
            self.timers.idle.notify_all();
        }
    }
}

/// Device grants resolve to this execution's narrow ports, never a VM lookup.
pub(crate) struct DevicePorts {
    control: Weak<ControlShared>,
    signals: Arc<RunSignals>,
    timers: Arc<DeviceTimers>,
    cpu_freq: Option<Arc<dyn GuestCpuFreqBackend>>,
}

impl DevicePorts {
    pub(crate) fn new(
        signals: Arc<RunSignals>,
        control: Weak<ControlShared>,
        cpu_freq: Option<Arc<dyn GuestCpuFreqBackend>>,
    ) -> Arc<Self> {
        let run = signals.run_id();
        let ports = Arc::new(Self {
            timers: DeviceTimers::new(signals.clone()),
            signals,
            control,
            cpu_freq,
        });
        if let Some(backend) = &ports.cpu_freq {
            backend.bind(run.vm().vm_id(), run.generation());
        }
        ports
    }

    pub(crate) fn access_ports(self: &Arc<Self>) -> RuntimeAccessPorts {
        RuntimeAccessPorts::new()
            .with_timer(self.clone())
            .with_wake(self.clone())
            .with_stop(self.clone())
            .with_work(self.clone())
            .with_cpufreq(self.clone())
    }

    pub(crate) fn close(&self) {
        self.retire_cpu_freq_hints();
        self.release_cpu_freq_binding();
        self.signals.close_interrupts();
    }

    /// Retires this run's host CPU-frequency hints without closing device
    /// signals. Stop uses this before teardown so a failed cleanup cannot leave
    /// a retired run raising the host CPU-frequency floor.
    ///
    /// The binding is deliberately kept: the guest may still read the current
    /// performance while its vCPUs are stopping, and an error on that read is
    /// reported as a vCPU fault.
    pub(crate) fn retire_cpu_freq_hints(&self) {
        let run = self.signals.run_id();
        if let Some(backend) = &self.cpu_freq {
            backend.retire(run.vm().vm_id(), run.generation());
        }
    }

    /// Releases this run's host CPU-frequency binding. Only valid once every
    /// vCPU of the run has returned and retired, so no device access can
    /// observe the missing binding.
    pub(crate) fn release_cpu_freq_binding(&self) {
        let run = self.signals.run_id();
        if let Some(backend) = &self.cpu_freq {
            backend.unbind(run.vm().vm_id(), run.generation());
        }
    }

    pub(crate) fn suspend(&self) -> AxVmResult {
        // Publish the host-side exclusion even when a timer worker reports a
        // suspend failure. A partially quiesced VM must never keep raising its
        // CPU-frequency floor while lifecycle rollback is in progress.
        let timer_result = self.timers.suspend();
        if let Some(backend) = &self.cpu_freq {
            let run = self.signals.run_id();
            backend.suspend(run.vm().vm_id(), run.generation());
        }
        timer_result
    }

    pub(crate) fn resume(&self) -> AxVmResult {
        self.timers.resume()?;
        if let Some(backend) = &self.cpu_freq {
            let run = self.signals.run_id();
            backend.resume(run.vm().vm_id(), run.generation());
        }
        Ok(())
    }
}

impl GuestCpuFreqPort for DevicePorts {
    fn set_performance(
        &self,
        _device: DeviceId,
        vcpu_id: DeviceVcpuId,
        domain: u32,
        performance: u32,
    ) -> DeviceManagerResult {
        let backend = self.cpu_freq.as_ref().ok_or_else(|| {
            cpu_freq_port_error(
                "set guest CPU frequency hint",
                GuestCpuFreqError::Unavailable("no host CPU frequency backend"),
            )
        })?;
        let run = self.signals.run_id();
        backend
            .set_hint(
                run.vm().vm_id(),
                run.generation(),
                vcpu_id.as_usize(),
                domain,
                performance,
            )
            .map_err(|error| cpu_freq_port_error("set guest CPU frequency hint", error))
    }

    fn current_performance(
        &self,
        _device: DeviceId,
        vcpu_id: DeviceVcpuId,
        domain: u32,
    ) -> DeviceManagerResult<u32> {
        let backend = self.cpu_freq.as_ref().ok_or_else(|| {
            cpu_freq_port_error(
                "read guest CPU frequency performance",
                GuestCpuFreqError::Unavailable("no host CPU frequency backend"),
            )
        })?;
        let run = self.signals.run_id();
        backend
            .current_performance(
                run.vm().vm_id(),
                run.generation(),
                vcpu_id.as_usize(),
                domain,
            )
            .map_err(|error| cpu_freq_port_error("read guest CPU frequency performance", error))
    }
}

impl WorkAccessPort for DevicePorts {
    fn notify_work(&self, _device: DeviceId) -> DeviceManagerResult {
        self.signals
            .notify_work()
            .map_err(|error| port_error("notify device work", error))
    }
}

impl WakeAccessPort for DevicePorts {
    fn wake_vcpu(&self, _device: DeviceId, vcpu: usize) -> DeviceManagerResult {
        self.signals
            .notify_vcpu(vcpu)
            .map_err(|error| port_error("wake device vCPU", error))
    }
}

impl TimerAccessPort for DevicePorts {
    fn schedule_timer(&self, _device: DeviceId, deadline_ns: u64) -> DeviceManagerResult {
        self.timers
            .schedule(Duration::from_nanos(deadline_ns))
            .map_err(|error| port_error("schedule device timer", error))
    }
}

impl StopAccessPort for DevicePorts {
    fn request_vm_stop(&self, device: DeviceId, reason: &str) -> DeviceManagerResult {
        let control = self
            .control
            .upgrade()
            .ok_or_else(|| port_error("request device stop", "owner exited"))?;
        // The device callback can be a participant in the stop. It submits the
        // request and ends its observation without synchronously waiting on it.
        control
            .request_run_stop(
                self.signals.run_id(),
                StopReason::Fault(format!("device {device:?}: {reason}")),
            )
            .map_err(|error| port_error("request device stop", error))?;
        Ok(())
    }
}

fn port_error(operation: &'static str, error: impl std::fmt::Debug) -> DeviceManagerError {
    DeviceManagerError::InvalidState {
        operation,
        detail: format!("{error:?}"),
    }
}

fn cpu_freq_port_error(operation: &'static str, error: GuestCpuFreqError) -> DeviceManagerError {
    let detail = error.to_string();
    match error {
        GuestCpuFreqError::InvalidRequest(_) => {
            DeviceManagerError::InvalidInput { operation, detail }
        }
        GuestCpuFreqError::Unavailable(_) | GuestCpuFreqError::HardwareFailure(_) => {
            DeviceManagerError::InvalidState { operation, detail }
        }
    }
}

#[cfg(all(test, feature = "host-test"))]
mod cpu_freq_port_tests {
    use std::sync::{Arc, Mutex, Weak};

    use axdevice::DeviceManagerError;
    use axdevice_base::{DeviceId, DeviceVcpuId};

    use super::*;
    use crate::{GuestCpuFreqBackend, GuestCpuFreqError, RunId, VMId, VmKey};

    #[derive(Default)]
    struct RecordingBackend {
        events: Mutex<Vec<String>>,
    }

    impl RecordingBackend {
        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }

        fn push(&self, event: impl Into<String>) {
            self.events.lock().unwrap().push(event.into());
        }
    }

    impl GuestCpuFreqBackend for RecordingBackend {
        fn bind(&self, vm_id: VMId, run_generation: u64) {
            self.push(format!("bind {vm_id} {run_generation}"));
        }

        fn set_hint(
            &self,
            vm_id: VMId,
            run_generation: u64,
            vcpu_id: usize,
            domain: u32,
            performance: u32,
        ) -> Result<(), GuestCpuFreqError> {
            self.push(format!(
                "set {vm_id} {run_generation} {vcpu_id} {domain} {performance}"
            ));
            Ok(())
        }

        fn current_performance(
            &self,
            vm_id: VMId,
            run_generation: u64,
            vcpu_id: usize,
            domain: u32,
        ) -> Result<u32, GuestCpuFreqError> {
            self.push(format!(
                "current {vm_id} {run_generation} {vcpu_id} {domain}"
            ));
            Ok(512)
        }

        fn retire(&self, vm_id: VMId, run_generation: u64) {
            self.push(format!("retire {vm_id} {run_generation}"));
        }

        fn unbind(&self, vm_id: VMId, run_generation: u64) {
            self.push(format!("unbind {vm_id} {run_generation}"));
        }

        fn suspend(&self, vm_id: VMId, run_generation: u64) {
            self.push(format!("suspend {vm_id} {run_generation}"));
        }

        fn resume(&self, vm_id: VMId, run_generation: u64) {
            self.push(format!("resume {vm_id} {run_generation}"));
        }
    }

    struct RejectingBackend;

    impl GuestCpuFreqBackend for RejectingBackend {
        fn set_hint(
            &self,
            _vm_id: VMId,
            _run_generation: u64,
            _vcpu_id: usize,
            _domain: u32,
            _performance: u32,
        ) -> Result<(), GuestCpuFreqError> {
            Err(GuestCpuFreqError::InvalidRequest(
                "guest CPU frequency domain is invalid",
            ))
        }

        fn current_performance(
            &self,
            _vm_id: VMId,
            _run_generation: u64,
            _vcpu_id: usize,
            _domain: u32,
        ) -> Result<u32, GuestCpuFreqError> {
            Err(GuestCpuFreqError::Unavailable(
                "host CPU frequency worker is not ready",
            ))
        }

        fn retire(&self, _vm_id: VMId, _run_generation: u64) {}

        fn unbind(&self, _vm_id: VMId, _run_generation: u64) {}

        fn suspend(&self, _vm_id: VMId, _run_generation: u64) {}

        fn resume(&self, _vm_id: VMId, _run_generation: u64) {}
    }

    fn ports(generation: u64, backend: Option<Arc<dyn GuestCpuFreqBackend>>) -> Arc<DevicePorts> {
        let signals = RunSignals::new(RunId::new(VmKey::new(1, 1), generation), 1)
            .expect("valid run signals");
        DevicePorts::new(signals, Weak::new(), backend)
    }

    #[test]
    fn device_ports_bind_retire_suspend_and_resume_one_run_generation() {
        let backend = Arc::new(RecordingBackend::default());
        let ports = ports(7, Some(backend.clone()));
        ports
            .set_performance(DeviceId::new(1), DeviceVcpuId::new(0), 3, 900)
            .unwrap();
        assert_eq!(
            ports.current_performance(DeviceId::new(1), DeviceVcpuId::new(0), 3),
            Ok(512)
        );
        ports.retire_cpu_freq_hints();
        ports.retire_cpu_freq_hints();
        ports.suspend().unwrap();
        ports.resume().unwrap();
        ports.close();
        assert_eq!(
            backend.events(),
            [
                "bind 1 7",
                "set 1 7 0 3 900",
                "current 1 7 0 3",
                "retire 1 7",
                "retire 1 7",
                "suspend 1 7",
                "resume 1 7",
                "retire 1 7",
                "unbind 1 7",
            ]
        );
    }

    /// Stop retires the generation while its vCPUs may still be running guest
    /// code, so the binding must survive until `close()`.
    #[test]
    fn retiring_one_run_generation_keeps_its_binding_until_close() {
        let backend = Arc::new(RecordingBackend::default());
        let first = ports(1, Some(backend.clone()));
        first
            .set_performance(DeviceId::new(1), DeviceVcpuId::new(0), 1, 256)
            .unwrap();
        first.retire_cpu_freq_hints();
        let second = ports(2, Some(backend.clone()));
        second
            .set_performance(DeviceId::new(1), DeviceVcpuId::new(0), 1, 1024)
            .unwrap();
        second.close();
        first.close();
        assert_eq!(
            backend.events(),
            [
                "bind 1 1",
                "set 1 1 0 1 256",
                "retire 1 1",
                "bind 1 2",
                "set 1 2 0 1 1024",
                "retire 1 2",
                "unbind 1 2",
                "retire 1 1",
                "unbind 1 1",
            ]
        );
    }

    #[test]
    fn cpu_freq_port_maps_typed_backend_errors() {
        let ports = ports(1, Some(Arc::new(RejectingBackend)));
        assert!(matches!(
            ports.set_performance(DeviceId::new(1), DeviceVcpuId::new(0), 0, 256),
            Err(DeviceManagerError::InvalidInput { operation, .. })
                if operation == "set guest CPU frequency hint"
        ));
        assert!(matches!(
            ports.current_performance(DeviceId::new(1), DeviceVcpuId::new(0), 0),
            Err(DeviceManagerError::InvalidState { operation, .. })
                if operation == "read guest CPU frequency performance"
        ));
    }
}

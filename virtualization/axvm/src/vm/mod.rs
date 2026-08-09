// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Virtual machine state, resources, and lifecycle entry points.

use std::{
    alloc::Layout,
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    format,
    string::String,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    vec::Vec,
};

use ax_cpumask::CpuMask;
use ax_memory_addr::align_up_4k;
use ax_std::os::arceos::sync::IrqSafeMutex as Mutex;
use axaddrspace::{AddrSpace, NestedPageTableOps};
use axdevice::*;
use axdevice_base::*;
use axvm_types::*;

use crate::{
    arch::*, boot::*, config::*, host::paging::*, irq::model::*, layout::*, lifecycle::*,
    runtime::*, sync::MutexExt, vcpu::*, *,
};

pub(crate) mod boot;
pub(crate) mod memory;
pub(crate) mod prepare;
pub use memory::PreparedMemoryLayout;

const VM_ASPACE_BASE: usize = 0x0;
const VM_ASPACE_SIZE: usize = 0x7fff_ffff_f000;

/// A vCPU with architecture-independent interface.
type VCpu = AxVCpu<crate::arch::ArchVCpu>;
/// A reference to a vCPU.
pub(crate) type AxVCpuRef<A = crate::arch::ArchVCpu> = Arc<AxVCpu<A>>;
/// A reference to a VM.
pub type AxVMRef = Arc<AxVM>;

struct VmDmaAccess<'a> {
    vm: &'a AxVM,
}

impl DeviceAccess for VmDmaAccess<'_> {
    fn device_id(&self) -> DeviceId {
        DeviceId::new(0)
    }
    fn read_guest_memory(
        &mut self,
        _grant: &DmaGrant,
        addr: GuestPhysAddr,
        data: &mut [u8],
    ) -> DeviceResult {
        self.vm
            .read_from_guest(addr, data)
            .map_err(|error| axdevice_base::DeviceError::Backend {
                operation: "read guest memory for DMA",
                detail: std::format!("{error}"),
            })
    }
    fn write_guest_memory(
        &mut self,
        _grant: &DmaGrant,
        addr: GuestPhysAddr,
        data: &[u8],
    ) -> DeviceResult {
        self.vm
            .write_to_guest(addr, data)
            .map_err(|error| axdevice_base::DeviceError::Backend {
                operation: "write guest memory for DMA",
                detail: std::format!("{error}"),
            })
    }
}

/// Architecture-independent vCPU runtime metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VcpuSnapshot {
    /// vCPU identifier inside its VM.
    pub id: usize,
    /// Current AxVM wrapper state.
    pub state: VmVcpuState,
    /// Optional physical CPU affinity mask.
    pub phys_cpu_set: Option<usize>,
}

pub(crate) fn width_mask(width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => 0xff,
        AccessWidth::Word => 0xffff,
        AccessWidth::Dword => 0xffff_ffff,
        AccessWidth::Qword => usize::MAX,
    }
}

pub(crate) fn sign_extend_value(value: usize, width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => (value as i8) as isize as usize,
        AccessWidth::Word => (value as i16) as isize as usize,
        AccessWidth::Dword => (value as i32) as isize as usize,
        AccessWidth::Qword => value,
    }
}

fn write_guest_bytes_to_chunks(chunks: &mut [&mut [u8]], data: &[u8]) -> AxVmResult {
    if data.is_empty() {
        return Ok(());
    }

    let mut copied = 0;
    for chunk in chunks {
        let len = (data.len() - copied).min(chunk.len());
        if len == 0 {
            continue;
        }
        chunk[..len].copy_from_slice(&data[copied..copied + len]);
        crate::clean_dcache_range((chunk.as_ptr() as usize).into(), len);
        copied += len;
        if copied == data.len() {
            return Ok(());
        }
    }

    ax_err!(
        InvalidInput,
        "Insufficient guest memory to write the requested buffer"
    )
}

pub(crate) struct AxVMResources {
    // Todo: use more efficient lock.
    pub(crate) address_space: AddrSpace<ArchNestedPageTable>,
    nested_paging: NestedPagingConfig,
    memory_regions: Vec<VMMemoryRegion>,
    config: AxVMConfig,
    phys_cpu_ls: PhysCpuList,
    vcpu_list: Option<Box<[AxVCpuRef]>>,
    devices: Option<Arc<DeviceRuntime>>,
    interrupt_controller: Option<Arc<dyn axdevice_base::VirtualInterruptController>>,
    address_layout: Option<VmAddressLayout>,
    boot_description: GuestBootDescription,
    device_plan: crate::arch::ArchVmPlan,
}

unsafe impl Send for AxVMResources {}
unsafe impl Sync for AxVMResources {}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum PendingInterrupt {
    Normal(usize),
    External { vector: usize, physical_irq: usize },
}

/// Runtime-only resources owned by Running/Paused/Stopping lifecycle states.
pub(crate) struct VmRuntimeHandle {
    wait_queue: crate::WaitQueue,
    vcpu_task_list: Mutex<BTreeMap<usize, crate::AxTaskRef>>,
    cpu_on_start_acks: StdMutex<BTreeMap<usize, Arc<crate::runtime::vcpus::CpuOnStartAck>>>,
    cpu_off_exit_reservations: StdMutex<BTreeSet<usize>>,
    pending_interrupts: Mutex<BTreeMap<usize, Vec<PendingInterrupt>>>,
    irq_dispatcher: crate::runtime::VcpuIrqDispatcher,
    running_halting_vcpu_count: AtomicUsize,
    lifecycle_error: StdMutex<Option<AxVmError>>,
    deferred_reset_requested: AtomicBool,
}

pub(crate) fn dispatch_vcpu_interrupt_with(
    enqueue: impl FnOnce() -> AxVmResult<usize>,
    notify: impl FnOnce(),
    send_ipi: impl FnOnce(usize),
) -> AxVmResult {
    let pcpu_id = enqueue()?;
    notify();
    send_ipi(pcpu_id);
    Ok(())
}

fn pulse_interrupt_with_snapshot(
    snapshot: impl FnOnce() -> AxVmResult<Arc<dyn axdevice_base::VirtualInterruptController>>,
    irq_id: usize,
) -> AxVmResult {
    use axdevice_base::{ControllerInputId, InterruptTriggerMode};

    snapshot()?
        .wired_input(
            ControllerInputId::new(irq_id),
            InterruptTriggerMode::EdgeTriggered,
        )?
        .connect()?
        .pulse()?;
    Ok(())
}

impl VmRuntimeHandle {
    pub(crate) fn new() -> Self {
        Self {
            wait_queue: crate::WaitQueue::new(),
            vcpu_task_list: Mutex::new(BTreeMap::new()),
            cpu_on_start_acks: StdMutex::new(BTreeMap::new()),
            cpu_off_exit_reservations: StdMutex::new(BTreeSet::new()),
            pending_interrupts: Mutex::new(BTreeMap::new()),
            irq_dispatcher: crate::runtime::VcpuIrqDispatcher::new(),
            running_halting_vcpu_count: AtomicUsize::new(0),
            lifecycle_error: StdMutex::new(None),
            deferred_reset_requested: AtomicBool::new(false),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn has_vcpu_task(&self, vcpu_id: usize) -> bool {
        self.vcpu_task_list.lock().contains_key(&vcpu_id)
    }

    pub(crate) fn add_vcpu_task(&self, vcpu_id: usize, vcpu_task: crate::AxTaskRef) -> AxVmResult {
        let mut vcpu_task_list = self.vcpu_task_list.lock();
        if vcpu_task_list.contains_key(&vcpu_id) {
            return ax_err!(BadState, format!("vCPU {vcpu_id} task already exists"));
        }

        self.irq_dispatcher
            .register_vcpu_task(vcpu_id, vcpu_task.clone());
        vcpu_task_list.insert(vcpu_id, vcpu_task);
        drop(vcpu_task_list);

        self.pending_interrupts.lock().entry(vcpu_id).or_default();
        Ok(())
    }

    pub(crate) fn remove_cpu_on_start_ack(
        &self,
        vcpu_id: usize,
    ) -> Option<Arc<crate::runtime::vcpus::CpuOnStartAck>> {
        self.cpu_on_start_acks.lock_unpoisoned().remove(&vcpu_id)
    }

    pub(crate) fn remove_vcpu_task(&self, vcpu_id: usize) -> Option<crate::AxTaskRef> {
        self.pending_interrupts.lock().remove(&vcpu_id);
        self.irq_dispatcher.unregister_vcpu_task(vcpu_id);
        self.vcpu_task_list.lock().remove(&vcpu_id)
    }

    #[allow(dead_code)]
    pub(crate) fn insert_cpu_on_start_ack(
        &self,
        vcpu_id: usize,
        ack: Arc<crate::runtime::vcpus::CpuOnStartAck>,
    ) -> AxVmResult {
        let mut acks = self.cpu_on_start_acks.lock_unpoisoned();
        if acks.contains_key(&vcpu_id) {
            return ax_err!(
                AlreadyExists,
                format!("vCPU {vcpu_id} CPU_ON ack already exists")
            );
        }
        acks.insert(vcpu_id, ack);
        Ok(())
    }

    pub(crate) fn cpu_on_start_ack(
        &self,
        vcpu_id: usize,
    ) -> Option<Arc<crate::runtime::vcpus::CpuOnStartAck>> {
        self.cpu_on_start_acks
            .lock_unpoisoned()
            .get(&vcpu_id)
            .cloned()
    }

    pub(crate) fn queue_pending_interrupt(
        &self,
        vcpu_id: usize,
        interrupt: PendingInterrupt,
    ) -> AxVmResult<usize> {
        let task = self
            .vcpu_task_list
            .lock()
            .get(&vcpu_id)
            .cloned()
            .ok_or_else(|| ax_err_type!(NotFound, format!("vCPU {vcpu_id} task not found")))?;
        self.queue_pending_interrupt_for_cpu(vcpu_id, task.cpu_id() as usize, interrupt)
    }

    pub(crate) fn queue_pending_interrupt_for_cpu(
        &self,
        vcpu_id: usize,
        cpu_id: usize,
        interrupt: PendingInterrupt,
    ) -> AxVmResult<usize> {
        self.pending_interrupts
            .lock()
            .entry(vcpu_id)
            .or_default()
            .push(interrupt);
        Ok(cpu_id)
    }

    pub(crate) fn vcpu_cpu_id(&self, vcpu_id: usize) -> AxVmResult<usize> {
        self.vcpu_task_list
            .lock()
            .get(&vcpu_id)
            .map(|task| task.cpu_id() as usize)
            .ok_or_else(|| ax_err_type!(NotFound, format!("vCPU {vcpu_id} task not found")))
    }

    /// New delivery path: enqueue → notify → host IPI.
    ///
    /// The dispatcher releases its queue lock before this method notifies
    /// waiters or invokes the host IPI boundary.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "architecture interrupt routers dispatch in later modules"
        )
    )]
    pub(crate) fn dispatch_vcpu_interrupt(
        &self,
        vcpu_id: usize,
        interrupt: PendingVcpuInterrupt,
    ) -> AxVmResult {
        dispatch_vcpu_interrupt_with(
            || self.irq_dispatcher.enqueue(vcpu_id, interrupt),
            || self.notify_all(),
            crate::host::task::send_ipi,
        )
    }

    /// Called by the vCPU run loop to drain pending interrupts before
    /// entering the guest.
    pub(crate) fn irq_dispatcher(&self) -> &VcpuIrqDispatcher {
        &self.irq_dispatcher
    }

    pub(crate) fn drain_pending_interrupts(&self, vcpu_id: usize) -> Vec<PendingInterrupt> {
        self.pending_interrupts
            .lock()
            .get_mut(&vcpu_id)
            .map(std::mem::take)
            .unwrap_or_default()
    }

    pub(crate) fn wait(&self) {
        self.wait_queue.wait();
    }

    pub(crate) fn wait_until(&self, condition: impl Fn() -> bool) {
        self.wait_queue.wait_until(condition);
    }

    pub(crate) fn notify_one(&self) {
        self.wait_queue.notify_one(false);
    }

    pub(crate) fn notify_all(&self) {
        self.wait_queue.notify_all(false);
    }

    pub(crate) fn mark_vcpu_running(&self) {
        self.running_halting_vcpu_count
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn publish_cpu_on_start_success(&self, ack: &crate::runtime::vcpus::CpuOnStartAck) {
        self.mark_vcpu_running();
        ack.complete(Ok(()));
    }

    pub(crate) fn mark_vcpu_exiting(&self) -> bool {
        self.running_halting_vcpu_count
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                count.checked_sub(1)
            })
            == Ok(1)
    }

    pub(crate) fn record_lifecycle_error(&self, error: AxVmError) {
        let mut recorded = self.lifecycle_error.lock_unpoisoned();
        if recorded.is_none() {
            *recorded = Some(error);
        }
    }

    fn take_lifecycle_error(&self) -> Option<AxVmError> {
        self.lifecycle_error.lock_unpoisoned().take()
    }

    pub(crate) fn request_deferred_reset(&self) -> bool {
        !self.deferred_reset_requested.swap(true, Ordering::AcqRel)
    }

    pub(crate) fn take_deferred_reset_request(&self) -> bool {
        self.deferred_reset_requested.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn try_reserve_cpu_off(&self, vcpu_id: usize) -> bool {
        let reserved = self
            .running_halting_vcpu_count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count > 1).then_some(count - 1)
            })
            .is_ok();

        if reserved {
            self.cpu_off_exit_reservations
                .lock_unpoisoned()
                .insert(vcpu_id);
        }
        reserved
    }

    pub(crate) fn consume_cpu_off_reservation(&self, vcpu_id: usize) -> bool {
        self.cpu_off_exit_reservations
            .lock_unpoisoned()
            .remove(&vcpu_id)
    }

    pub(crate) fn join_all_vcpu_tasks(&self, vm_id: usize) -> AxVmResult {
        if self.vcpu_task_list.lock().is_empty() {
            return self.take_lifecycle_error().map_or(Ok(()), Err);
        }
        let current = crate::host::task::current_task();
        let tasks: Vec<_> = self
            .vcpu_task_list
            .lock()
            .values()
            .filter(|task| !current.ptr_eq(task))
            .cloned()
            .collect();
        let task_count = tasks.len();
        info!("VM[{vm_id}] Joining {task_count} VCpu tasks...");
        for (idx, task) in tasks.iter().enumerate() {
            debug!(
                "VM[{}] Joining VCpu task[{}]: {}",
                vm_id,
                idx,
                task.id_name()
            );
            let exit_code = task.join();
            debug!("VM[{vm_id}] VCpu task[{idx}] exited with code: {exit_code}");
        }
        info!("VM[{vm_id}] VCpu resources cleaned up, {task_count} VCpu tasks joined");
        self.take_lifecycle_error().map_or(Ok(()), Err)
    }
}

#[cfg(all(test, feature = "host-test"))]
mod runtime_handle_tests {

    #[test]
    fn runtime_cpu_on_success_publishes_online_count_before_ack() {
        let runtime = VmRuntimeHandle::new();
        let ack = crate::runtime::vcpus::CpuOnStartAck::new();

        runtime.mark_vcpu_running();
        assert!(ack.begin_startup());

        runtime.publish_cpu_on_start_success(&ack);

        assert!(ack.is_complete());
        assert!(runtime.try_reserve_cpu_off(0));
    }

    #[test]
    fn runtime_cpu_on_ack_rejects_duplicate_and_can_be_removed() {
        let runtime = VmRuntimeHandle::new();
        let first = Arc::new(crate::runtime::vcpus::CpuOnStartAck::new());
        let second = Arc::new(crate::runtime::vcpus::CpuOnStartAck::new());

        assert!(runtime.insert_cpu_on_start_ack(1, first.clone()).is_ok());
        assert!(runtime.insert_cpu_on_start_ack(1, second).is_err());

        let stored = runtime.cpu_on_start_ack(1).unwrap();
        assert!(Arc::ptr_eq(&stored, &first));

        assert!(runtime.remove_cpu_on_start_ack(1).is_some());
        assert!(runtime.cpu_on_start_ack(1).is_none());
    }

    #[test]
    fn runtime_deferred_reset_request_is_single_consumer() {
        let runtime = VmRuntimeHandle::new();

        assert!(!runtime.take_deferred_reset_request());
        assert!(runtime.request_deferred_reset());
        assert!(!runtime.request_deferred_reset());
        assert!(runtime.take_deferred_reset_request());
        assert!(!runtime.take_deferred_reset_request());
        assert!(runtime.request_deferred_reset());
    }

    #[test]
    fn runtime_cpu_off_reservation_rejects_second_parallel_last_vcpu() {
        let runtime = VmRuntimeHandle::new();

        runtime.mark_vcpu_running();
        runtime.mark_vcpu_running();

        assert!(runtime.try_reserve_cpu_off(0));
        assert!(!runtime.try_reserve_cpu_off(1));

        assert!(runtime.consume_cpu_off_reservation(0));
        assert!(!runtime.consume_cpu_off_reservation(0));

        assert!(runtime.mark_vcpu_exiting());
    }

    use super::*;

    #[test]
    fn remove_vcpu_task_clears_pending_interrupts_and_dispatcher_registration() {
        let runtime = VmRuntimeHandle::new();

        runtime.pending_interrupts.lock().entry(3).or_default();
        runtime.irq_dispatcher.register_test_vcpu(3, 11);

        assert!(runtime.pending_interrupts.lock().contains_key(&3));
        assert_eq!(runtime.irq_dispatcher.test_lookup_cpu_id(3).unwrap(), 11);

        runtime.remove_vcpu_task(3);

        assert!(!runtime.pending_interrupts.lock().contains_key(&3));
        assert!(runtime.irq_dispatcher.test_lookup_cpu_id(3).is_err());

        runtime.remove_vcpu_task(3);
        assert!(!runtime.pending_interrupts.lock().contains_key(&3));
        assert!(runtime.irq_dispatcher.test_lookup_cpu_id(3).is_err());
    }
}

impl AxVMResources {
    pub(crate) fn from_page_table(
        config: AxVMConfig,
        page_table: ArchNestedPageTable,
        device_plan: crate::arch::ArchVmPlan,
        build_nested_paging: impl FnOnce(HostPhysAddr) -> AxVmResult<NestedPagingConfig>,
    ) -> AxVmResult<Self> {
        let address_space = AddrSpace::new_empty(
            page_table,
            GuestPhysAddr::from(VM_ASPACE_BASE),
            VM_ASPACE_SIZE,
        )
        .map_err(|error| AxVmError::from_addrspace("create guest address space", error))?;
        let nested_paging = build_nested_paging(address_space.page_table_root())?;
        Ok(Self {
            address_space,
            nested_paging,
            memory_regions: Vec::new(),
            config,
            phys_cpu_ls: PhysCpuList::default(),
            vcpu_list: None,
            devices: None,
            interrupt_controller: None,
            address_layout: None,
            boot_description: GuestBootDescription::none(),
            device_plan,
        })
    }

    #[cfg(not(target_arch = "x86_64"))]
    pub(crate) const fn config(&self) -> &AxVMConfig {
        &self.config
    }

    pub(crate) fn planned_devices(&self) -> &crate::vm::prepare::device_plan::VmDevicePlan {
        use crate::vm::prepare::device_plan::ArchitectureVmPlan;

        self.device_plan.devices()
    }

    #[cfg(target_arch = "aarch64")]
    pub(crate) const fn architecture_plan(&self) -> &crate::arch::ArchVmPlan {
        &self.device_plan
    }

    fn vcpu_list(&self) -> AxVmResult<&[AxVCpuRef]> {
        self.vcpu_list
            .as_deref()
            .ok_or_else(|| ax_err_type!(BadState, "VM vCPU resources are not prepared"))
    }

    fn devices(&self) -> AxVmResult<Arc<DeviceRuntime>> {
        self.devices
            .clone()
            .ok_or_else(|| ax_err_type!(BadState, "VM devices are not prepared"))
    }

    fn interrupt_controller(
        &self,
    ) -> AxVmResult<&Arc<dyn axdevice_base::VirtualInterruptController>> {
        self.interrupt_controller
            .as_ref()
            .ok_or_else(|| ax_err_type!(BadState, "VM interrupt controller is not prepared"))
    }

    fn reset_transient_resources(&mut self) -> AxVmResult {
        if let Some(devices) = self.devices.take() {
            devices
                .reset_lifecycle_devices()
                .map_err(|error| AxVmError::device("reset device lifecycle", error))?;
        }
        let memory_regions = self.memory_regions.clone();
        self.address_space.clear();
        for region in &memory_regions {
            self.address_space
                .map_linear(
                    region.gpa,
                    region.host_paddr(),
                    region.size(),
                    MappingFlags::READ
                        | MappingFlags::WRITE
                        | MappingFlags::EXECUTE
                        | MappingFlags::USER,
                )
                .map_err(|error| {
                    AxVmError::from_addrspace("restore guest memory mapping", error)
                })?;
        }
        self.vcpu_list = None;
        self.interrupt_controller = None;
        self.address_layout = None;
        Ok(())
    }
}

pub struct FwCfgDeviceConfig {
    pub base: GuestPhysAddr,
    pub size: usize,
    pub kernel: FwCfgKernelPayload,
    pub initrd: Option<Arc<[u8]>>,
    pub cmdline: Option<String>,
    pub cpu_num: u16,
    pub platform: FwCfgPlatformConfig,
}

#[derive(Clone)]
struct AxVmDeviceAccessPorts {
    vm_id: usize,
}

impl AxVmDeviceAccessPorts {
    const fn new(vm_id: usize) -> Self {
        Self { vm_id }
    }

    fn into_ports(self) -> RuntimeAccessPorts {
        let this = Arc::new(self);
        RuntimeAccessPorts::new()
            .with_timer(this.clone())
            .with_wake(this.clone())
            .with_stop(this)
    }

    fn vm(&self, operation: &'static str) -> axdevice::DeviceManagerResult<AxVMRef> {
        crate::get_vm_by_id(self.vm_id).ok_or_else(|| {
            axdevice::DeviceManagerError::ResourceNotFound {
                operation,
                resource: format!("VM[{}]", self.vm_id),
            }
        })
    }
}

impl TimerAccessPort for AxVmDeviceAccessPorts {
    fn schedule_timer(
        &self,
        device_id: DeviceId,
        deadline_ns: u64,
    ) -> axdevice::DeviceManagerResult {
        let vm_id = self.vm_id;
        trace!(
            "VM[{vm_id}] device {device_id:?} scheduled access-scoped timer at {deadline_ns:#x} ns"
        );
        crate::timer::register_timer(
            deadline_ns,
            Box::new(move |_| crate::runtime::vcpus::notify_all_vcpus(vm_id)),
        );
        Ok(())
    }
}

impl WakeAccessPort for AxVmDeviceAccessPorts {
    fn wake_vcpu(&self, device_id: DeviceId, vcpu_id: usize) -> axdevice::DeviceManagerResult {
        let vm = self.vm("wake vCPU from device access")?;
        if vm.vcpu(vcpu_id).is_none() {
            return Err(axdevice::DeviceManagerError::InvalidInput {
                operation: "wake vCPU from device access",
                detail: format!(
                    "device {device_id:?} requested nonexistent VM[{}] vCPU {}",
                    self.vm_id, vcpu_id
                ),
            });
        }
        vm.with_runtime(|runtime| {
            runtime.notify_all();
            Ok(())
        })
        .map_err(|error| axdevice::DeviceManagerError::InvalidState {
            operation: "wake vCPU from device access",
            detail: format!("{error}"),
        })
    }
}

impl StopAccessPort for AxVmDeviceAccessPorts {
    fn request_vm_stop(&self, device_id: DeviceId, reason: &str) -> axdevice::DeviceManagerResult {
        let vm = self.vm("request VM stop from device access")?;
        vm.stop(StopReason::Fault(format!(
            "device {device_id:?} requested VM stop: {reason}"
        )))
        .map_err(|error| axdevice::DeviceManagerError::InvalidState {
            operation: "request VM stop from device access",
            detail: format!("{error}"),
        })?;
        if let Ok(()) = vm.with_runtime(|runtime| {
            runtime.notify_all();
            Ok(())
        }) {}
        Ok(())
    }
}

/// Represents a memory region in a virtual machine.
#[derive(Debug, Clone)]
pub struct VMMemoryRegion {
    /// Guest physical address.
    pub gpa: GuestPhysAddr,
    /// Host virtual address.
    pub hva: HostVirtAddr,
    /// Memory layout of the region.
    pub layout: Layout,
    /// Whether this region was allocated by the allocator and needs to be deallocated
    pub needs_dealloc: bool,
}

impl VMMemoryRegion {
    /// Returns the size of the memory region.
    pub fn size(&self) -> usize {
        self.layout.size()
    }

    /// Returns the host physical address backing this guest memory region.
    pub fn host_paddr(&self) -> HostPhysAddr {
        virt_to_phys(self.hva)
    }

    /// Returns `true` if the guest physical address is identical to the host physical address.
    pub fn is_identical(&self) -> bool {
        self.gpa.as_usize() == self.host_paddr().as_usize()
    }
}

const TEMP_MAX_VCPU_NUM: usize = 64;

/// A Virtual Machine.
pub struct AxVM {
    id: usize,
    name: String,
    machine: Mutex<Machine<AxVMResources, Arc<VmRuntimeHandle>>>,
    fw_cfg_payload: Arc<FwCfgPayloadSlot>,
}

impl AxVM {
    /// Creates a ready VM with eagerly initialized architecture resources.
    ///
    /// The VM is not started until [`Self::start`] is called.
    ///
    /// # Errors
    ///
    /// Returns an error if nested paging is unsupported for the selected host
    /// CPUs or if the initial stage-2 address space cannot be allocated.
    pub fn new(config: AxVMConfig) -> AxVmResult<AxVMRef> {
        let id = config.id();
        let name = config.name();
        let fw_cfg_payload = Arc::new(FwCfgPayloadSlot::new());
        let resources =
            crate::arch::CurrentArch::create_vm_resources(config, fw_cfg_payload.clone())?;
        let result = Arc::new(Self {
            id,
            name,
            machine: Mutex::new(Machine::Ready(resources)),
            fw_cfg_payload,
        });

        info!("VM created: id={}", result.id());

        Ok(result)
    }

    /// Returns the VM id.
    #[inline]
    pub fn id(&self) -> usize {
        self.id
    }

    /// Returns the configured VM name.
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// Returns the current lifecycle status.
    pub fn status(&self) -> VmStatus {
        self.machine.lock().status()
    }

    /// Returns whether the guest address space starts from host identity mappings.
    pub fn uses_passthrough_address_space(&self) -> bool {
        self.with_resources(|resources| Ok(resources.config.uses_passthrough_address_space()))
            .unwrap_or(false)
    }

    fn with_resources<F, R>(&self, f: F) -> AxVmResult<R>
    where
        F: FnOnce(&AxVMResources) -> AxVmResult<R>,
    {
        let machine = self.machine.lock();
        let resources = machine
            .resources()
            .ok_or_else(|| ax_err_type!(BadState, "VM resources are not available"))?;
        f(resources)
    }

    fn with_resources_mut<F, R>(&self, f: F) -> AxVmResult<R>
    where
        F: FnOnce(&mut AxVMResources) -> AxVmResult<R>,
    {
        let mut machine = self.machine.lock();
        let resources = machine
            .resources_mut()
            .ok_or_else(|| ax_err_type!(BadState, "VM resources are not available"))?;
        f(resources)
    }

    /// Snapshots the interrupt backend while validating the VM lifecycle state.
    ///
    /// The snapshot pins only the backend capability. A lifecycle change after
    /// this method returns is observed by the sender when it resolves the
    /// current runtime.
    fn interrupt_controller_snapshot(
        &self,
    ) -> AxVmResult<Arc<dyn axdevice_base::VirtualInterruptController>> {
        let machine = self.machine.lock();
        match machine.status() {
            VmStatus::Running | VmStatus::Paused => machine
                .resources()
                .ok_or_else(|| ax_err_type!(BadState, "VM resources are not available"))?
                .interrupt_controller()
                .cloned(),
            status => ax_err!(
                BadState,
                format!("VM[{}] cannot accept IRQ in {status:?}", self.id())
            ),
        }
    }

    pub(crate) fn with_runtime<F, R>(&self, f: F) -> AxVmResult<R>
    where
        F: FnOnce(&Arc<VmRuntimeHandle>) -> AxVmResult<R>,
    {
        let machine = self.machine.lock();
        let runtime = machine
            .runtime()
            .ok_or_else(|| ax_err_type!(BadState, "VM runtime is not available"))?;
        f(runtime)
    }

    pub(crate) fn current_interrupt_runtime(&self) -> AxVmResult<Arc<VmRuntimeHandle>> {
        let machine = self.machine.lock();
        Ok(machine.interrupt_runtime()?.clone())
    }

    fn take_stopped_runtime(&self) -> Option<Arc<VmRuntimeHandle>> {
        self.machine.lock().take_stopped_runtime()
    }

    /// Retrieves the vCPU corresponding to the given vcpu_id for the VM.
    /// Returns None if the vCPU does not exist.
    #[inline]
    pub(crate) fn vcpu(&self, vcpu_id: usize) -> Option<AxVCpuRef> {
        self.vcpu_list().get(vcpu_id).cloned()
    }

    /// Returns the number of vCPUs corresponding to the VM.
    #[inline]
    pub fn vcpu_num(&self) -> usize {
        self.with_resources(|resources| Ok(resources.vcpu_list().map_or(0, <[_]>::len)))
            .unwrap_or(0)
    }

    /// Returns a snapshot of the VM's vCPU references.
    #[inline]
    pub(crate) fn vcpu_list(&self) -> Vec<AxVCpuRef> {
        self.with_resources(|resources| Ok(resources.vcpu_list()?.to_vec()))
            .unwrap_or_default()
    }

    /// Returns architecture-independent vCPU metadata snapshots.
    pub fn vcpu_snapshots(&self) -> Vec<VcpuSnapshot> {
        self.vcpu_list()
            .iter()
            .map(|vcpu| VcpuSnapshot {
                id: vcpu.id(),
                state: vcpu.state(),
                phys_cpu_set: vcpu.phys_cpu_set(),
            })
            .collect()
    }

    /// Returns the root address of the nested page table for the VM.
    pub fn nested_page_table_root(&self) -> AxVmResult<HostPhysAddr> {
        self.with_resources(|resources| Ok(resources.address_space.page_table_root()))
    }

    /// Returns to the VM's configuration.
    pub fn with_config<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut AxVMConfig) -> R,
    {
        let mut machine = self.machine.lock();
        let resources = machine
            .resources_mut()
            .expect("VM resources are not available for config access");
        f(&mut resources.config)
    }

    #[cfg(target_arch = "aarch64")]
    pub(crate) fn with_architecture_plan<F, R>(&self, f: F) -> AxVmResult<R>
    where
        F: FnOnce(&crate::arch::ArchVmPlan) -> AxVmResult<R>,
    {
        self.with_resources(|resources| f(resources.architecture_plan()))
    }

    /// Reads the immutable device graph resolved during architecture planning.
    pub(crate) fn with_planned_device_graph<F, R>(&self, f: F) -> AxVmResult<R>
    where
        F: FnOnce(&axdevice::ResolvedDeviceGraph) -> AxVmResult<R>,
    {
        self.with_resources(|resources| f(resources.planned_devices().graph()))
    }

    /// Stores a guest DTB as VM-owned boot-description state.
    pub fn set_guest_device_tree(&self, load_gpa: GuestPhysAddr, bytes: Vec<u8>) -> AxVmResult {
        self.with_resources_mut(|resources| {
            resources.config.set_dtb_load_gpa(load_gpa);
            resources
                .boot_description
                .set_device_tree(GuestFdtBuilder::from_bytes(bytes).build(load_gpa));
            Ok(())
        })
    }

    /// Stores a directly installed guest ACPI image as VM-owned boot state.
    pub fn set_guest_acpi_tables(&self, rsdp_gpa: GuestPhysAddr, bytes: Vec<u8>) -> AxVmResult {
        self.with_resources_mut(|resources| {
            resources
                .boot_description
                .set_acpi_tables(GuestAcpiTables::generated(rsdp_gpa, bytes));
            Ok(())
        })
    }

    /// Returns guest VM image load region in `Vec<&'static mut [u8]>`,
    /// according to the given `image_load_gpa` and `image_size.
    /// `Vec<&'static mut [u8]>` is a series of (HVA) address segments,
    /// which may correspond to non-contiguous physical addresses,
    ///
    /// FIXME:
    /// Find a more elegant way to manage potentially non-contiguous physical memory
    ///         instead of `Vec<&'static mut [u8]>`.
    pub fn get_image_load_region(
        &self,
        image_load_gpa: GuestPhysAddr,
        image_size: usize,
    ) -> AxVmResult<Vec<&'static mut [u8]>> {
        let image_load_hva = self.with_resources(|resources| {
            resources
                .address_space
                .translated_byte_buffer(image_load_gpa, image_size)
                .ok_or_else(|| {
                    ax_err_type!(BadState, "Failed to translate kernel image load address")
                })
        })?;
        Ok(image_load_hva)
    }

    /// Starts the VM by transitioning to Running state.
    pub fn start(self: &Arc<Self>) -> AxVmResult {
        if self.status() == VmStatus::Stopped {
            if let Some(runtime) = self.take_stopped_runtime() {
                runtime.join_all_vcpu_tasks(self.id())?;
            }
            crate::arch::CurrentArch::deactivate_devices(self)?;
            self.prepare()?;
        }
        info!("Starting VM[{}]", self.id());
        let primary_vcpu = self
            .vcpu(0)
            .ok_or_else(|| ax_err_type!(BadState, "VM primary vCPU is not prepared"))?;
        let primary_task = crate::runtime::vcpus::build_vcpu_task(self, primary_vcpu);
        let runtime = Arc::new(VmRuntimeHandle::new());

        self.with_resources(|resources| {
            resources
                .vcpu_list()
                .map_err(|error| AxVmError::resource_unavailable("vCPU list", error))?;
            resources
                .devices()
                .map_err(|error| AxVmError::resource_unavailable("devices", error))?;
            resources
                .interrupt_controller()
                .map_err(|error| AxVmError::resource_unavailable("interrupt controller", error))?;
            Ok(())
        })?;

        crate::arch::CurrentArch::activate_devices(self)?;
        let start_result = self
            .machine
            .lock()
            .start_with(|_resources| Ok(runtime.clone()));
        if let Err(error) = start_result {
            return match crate::arch::CurrentArch::deactivate_devices(self) {
                Ok(()) => Err(error),
                Err(rollback) => Err(AxVmError::lifecycle_rollback("start VM", error, rollback)),
            };
        }

        let task = crate::host::task::spawn_task(primary_task);
        runtime.add_vcpu_task(0, task)?;
        Ok(())
    }

    /// Returns if the VM is running.
    pub fn running(&self) -> bool {
        self.status() == VmStatus::Running
    }

    /// Returns if the VM is shutting down (in Stopping state).
    pub fn stopping(&self) -> bool {
        self.status() == VmStatus::Stopping
    }

    /// Returns if the VM is suspended.
    pub fn suspending(&self) -> bool {
        matches!(self.status(), VmStatus::Pausing | VmStatus::Paused)
    }

    /// Returns if the VM is stopped.
    pub fn stopped(&self) -> bool {
        self.status() == VmStatus::Stopped
    }

    /// Pauses a running VM.
    pub fn pause(&self) -> AxVmResult {
        let mut machine = self.machine.lock();
        if machine.status() != VmStatus::Running {
            return machine.pause();
        }
        let devices = machine
            .resources()
            .expect("running VM must retain resources")
            .devices()?;
        devices
            .suspend_lifecycle_devices()
            .map_err(|error| AxVmError::device("suspend device lifecycle", error))?;
        machine.pause()
    }

    /// Resumes a paused VM.
    pub fn resume(&self) -> AxVmResult {
        let mut machine = self.machine.lock();
        if machine.status() != VmStatus::Paused {
            return machine.resume();
        }
        let devices = machine
            .resources()
            .expect("paused VM must retain resources")
            .devices()?;
        devices
            .resume_lifecycle_devices()
            .map_err(|error| AxVmError::device("resume device lifecycle", error))?;
        machine.resume()
    }

    /// Requests a stop. Running vCPUs observe the Stopping state and exit.
    pub fn stop(&self, reason: StopReason) -> AxVmResult {
        info!("Stopping VM[{}]: {reason:?}", self.id());
        self.machine.lock().request_stop_with(reason, |_, _| Ok(()))
    }

    pub(crate) fn finish_stop(&self) -> AxVmResult {
        self.machine.lock().finish_stop()
    }

    fn wait_until_stopped(&self) -> AxVmResult {
        const MAX_YIELDS: usize = 10_000;
        for _ in 0..MAX_YIELDS {
            match self.status() {
                VmStatus::Stopped | VmStatus::Ready => return Ok(()),
                VmStatus::Stopping | VmStatus::Running | VmStatus::Paused | VmStatus::Pausing => {
                    crate::host::task::yield_now();
                }
                status => {
                    return ax_err!(
                        BadState,
                        format!("VM[{}] cannot wait for stop from {status:?}", self.id())
                    );
                }
            }
        }
        ax_err!(
            BadState,
            format!("VM[{}] did not stop before reset timeout", self.id())
        )
    }

    fn stop_and_join_runtime(&self, reason: StopReason) -> AxVmResult {
        match self.status() {
            VmStatus::Running | VmStatus::Paused => {
                self.stop(reason)?;
                if let Ok(()) = self.with_runtime(|runtime| {
                    runtime.notify_all();
                    Ok(())
                }) {}
                self.wait_until_stopped()?;
            }
            VmStatus::Stopping => {
                if let Ok(()) = self.with_runtime(|runtime| {
                    runtime.notify_all();
                    Ok(())
                }) {}
                self.wait_until_stopped()?;
            }
            VmStatus::Stopped | VmStatus::Ready => {}
            status => {
                return ax_err!(
                    BadState,
                    format!("VM[{}] cannot quiesce runtime from {status:?}", self.id())
                );
            }
        }

        if let Some(runtime) = self.take_stopped_runtime() {
            runtime.join_all_vcpu_tasks(self.id())?;
        }
        crate::arch::CurrentArch::deactivate_devices(self)
    }

    /// Resets the VM by discarding runtime-only state, rebuilding vCPUs/devices,
    /// and starting from a fresh `Running` state.
    pub fn reset(self: &Arc<Self>) -> AxVmResult {
        info!("Resetting VM[{}]", self.id());
        self.stop_and_join_runtime(StopReason::Forced)?;

        self.machine.lock().reset_with(|resources| {
            resources
                .reset_transient_resources()
                .map_err(|error| AxVmError::resource_unavailable("reset resources", error))
        })?;
        self.prepare()?;
        self.start()
    }

    /// Returns this VM's emulated devices.
    pub fn get_devices(&self) -> AxVmResult<Arc<DeviceRuntime>> {
        self.with_resources(|resources| resources.devices())
    }

    /// Pulses a prepared VM interrupt-controller input without exposing it.
    pub fn pulse_interrupt(&self, irq_id: usize) -> AxVmResult {
        pulse_interrupt_with_snapshot(|| self.interrupt_controller_snapshot(), irq_id)
    }

    /// Returns the number of prepared emulated devices.
    pub fn device_count(&self) -> usize {
        self.get_devices()
            .map(|devices| devices.devices().count())
            .unwrap_or(0)
    }

    /// Queue a QEMU fw_cfg device that will be attached during VM initialization.
    pub fn add_fw_cfg_device(&self, config: FwCfgDeviceConfig) -> AxVmResult {
        let base = config.base;
        let size = config.size;
        let kernel_size = config.kernel.total_len();
        let initrd_size = config.initrd.as_ref().map(|data| data.len());
        self.fw_cfg_payload.set(FwCfgPayloadConfig {
            base: config.base,
            size: config.size,
            kernel: config.kernel,
            initrd: config.initrd,
            cmdline: config.cmdline,
            cpu_num: config.cpu_num,
            platform: config.platform,
        })?;
        debug!(
            "VM[{}] queued fw_cfg device: base={:#x}, size={:#x}, kernel={} bytes, initrd={:?}",
            self.id(),
            base.as_usize(),
            size,
            kernel_size,
            initrd_size
        );
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn fw_cfg_payload(&self) -> Option<FwCfgPayloadConfig> {
        self.fw_cfg_payload.get()
    }

    /// Builds the runtime ports used by access-scoped device grants.
    pub(crate) fn device_access_ports(&self) -> RuntimeAccessPorts {
        AxVmDeviceAccessPorts::new(self.id()).into_ports()
    }

    pub(crate) fn try_handle_mmio_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        data: usize,
    ) -> AxVmResult<bool> {
        let devices = self.get_devices()?;
        let mut memory = VmDmaAccess { vm: self };
        devices
            .try_handle_mmio_write_with_memory(addr, width, data, &mut memory)
            .map_err(Into::into)
    }

    pub(crate) fn try_handle_port_write(
        &self,
        port: Port,
        width: AccessWidth,
        data: usize,
    ) -> AxVmResult<bool> {
        let devices = self.get_devices()?;
        let mut memory = VmDmaAccess { vm: self };
        devices
            .try_handle_port_write_with_memory(port, width, data, &mut memory)
            .map_err(Into::into)
    }

    pub(crate) fn handle_nested_page_fault(
        &self,
        addr: GuestPhysAddr,
        access_flags: MappingFlags,
    ) -> bool {
        self.with_resources_mut(|resources| {
            let handled = resources
                .address_space
                .handle_page_fault(addr, access_flags);
            Self::debug_nested_page_fault(self.id(), resources, addr, access_flags, handled);
            Ok(handled)
        })
        .unwrap_or(false)
    }

    fn debug_nested_page_fault(
        vm_id: usize,
        resources: &AxVMResources,
        addr: GuestPhysAddr,
        access_flags: MappingFlags,
        handled: bool,
    ) {
        let root = resources.address_space.page_table_root();
        match NestedPageTableOps::query(resources.address_space.page_table(), addr) {
            Ok((hpa, flags, size)) => {
                if handled {
                    debug!(
                        "VM[{}] stage2 query hit: gpa={:#x} -> hpa={:#x}, access={:?}, \
                         pte_flags={:?}, page_size={:?}, root={:#x}",
                        vm_id,
                        addr.as_usize(),
                        hpa.as_usize(),
                        access_flags,
                        flags,
                        size,
                        root.as_usize()
                    );
                } else {
                    warn!(
                        "VM[{}] stage2 query hit: gpa={:#x} -> hpa={:#x}, access={:?}, \
                         pte_flags={:?}, page_size={:?}, root={:#x}",
                        vm_id,
                        addr.as_usize(),
                        hpa.as_usize(),
                        access_flags,
                        flags,
                        size,
                        root.as_usize()
                    );
                }
            }
            Err(err) => {
                if handled {
                    debug!(
                        "VM[{}] stage2 query miss: gpa={:#x}, access={:?}, err={:?}, root={:#x}",
                        vm_id,
                        addr.as_usize(),
                        access_flags,
                        err,
                        root.as_usize()
                    );
                } else {
                    warn!(
                        "VM[{}] stage2 query miss: gpa={:#x}, access={:?}, err={:?}, root={:#x}",
                        vm_id,
                        addr.as_usize(),
                        access_flags,
                        err,
                        root.as_usize()
                    );
                }
            }
        }

        let translate = resources.address_space.translate(addr);
        if handled {
            debug!(
                "VM[{}] stage2 translate: gpa={:#x} -> {:?}",
                vm_id,
                addr.as_usize(),
                translate
            );
        } else {
            warn!(
                "VM[{}] stage2 translate: gpa={:#x} -> {:?}",
                vm_id,
                addr.as_usize(),
                translate
            );
        }

        for (idx, region) in resources.memory_regions.iter().enumerate() {
            let start = region.gpa.as_usize();
            let end = start + region.size();
            if (start..end).contains(&addr.as_usize()) {
                if handled {
                    debug!(
                        "VM[{}] stage2 region hit[{}]: gpa=[{:#x},{:#x}) hva={:#x} hpa={:#x} \
                         size={:#x} identical={}",
                        vm_id,
                        idx,
                        start,
                        end,
                        region.hva.as_usize(),
                        region.host_paddr().as_usize(),
                        region.size(),
                        region.is_identical()
                    );
                } else {
                    warn!(
                        "VM[{}] stage2 region hit[{}]: gpa=[{:#x},{:#x}) hva={:#x} hpa={:#x} \
                         size={:#x} identical={}",
                        vm_id,
                        idx,
                        start,
                        end,
                        region.hva.as_usize(),
                        region.host_paddr().as_usize(),
                        region.size(),
                        region.is_identical()
                    );
                }
            }
        }
    }

    /// Injects an interrupt to the vCPU.
    pub fn inject_interrupt_to_vcpu(
        &self,
        targets: CpuMask<TEMP_MAX_VCPU_NUM>,
        irq: usize,
    ) -> AxVmResult {
        for vcpu in self.vcpu_list() {
            if targets.get(vcpu.id()) {
                crate::runtime::vcpus::queue_interrupt(self.id(), vcpu.id(), irq)?;
            }
        }
        Ok(())
    }

    /// Returns vCpu id list and its corresponding pCpu affinity list, as well as its physical id.
    /// If the pCpu affinity is None, it means the vCpu will be allocated to any available pCpu randomly.
    /// if the pCPU id is not provided, the vCpu's physical id will be set as vCpu id.
    ///
    /// Returns a vector of tuples, each tuple contains:
    /// - The vCpu id.
    /// - The pCpu affinity mask, `None` if not set.
    /// - The physical id of the vCpu, equal to vCpu id if not provided.
    pub fn get_vcpu_affinities_pcpu_ids(&self) -> Vec<(usize, Option<usize>, usize)> {
        self.with_resources(|resources| Ok(resources.phys_cpu_ls.get_vcpu_affinities_pcpu_ids()))
            .unwrap_or_default()
    }

    pub fn get_vcpu_guest_mpidrs(&self) -> Vec<(usize, u64)> {
        self.vcpu_list()
            .iter()
            .filter_map(|vcpu| vcpu.guest_mpidr().map(|mpidr| (vcpu.id(), mpidr)))
            .collect()
    }

    /// Maps a region of host physical memory to guest physical memory.
    pub fn map_region(
        &self,
        gpa: GuestPhysAddr,
        hpa: HostPhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> AxVmResult {
        self.with_resources_mut(|resources| {
            resources
                .address_space
                .map_linear(gpa, hpa, size, flags)
                .map_err(|error| AxVmError::from_addrspace("map guest memory region", error))?;
            Ok(())
        })
    }

    /// Unmaps a region of guest physical memory.
    pub fn unmap_region(&self, gpa: GuestPhysAddr, size: usize) -> AxVmResult {
        self.with_resources_mut(|resources| {
            resources
                .address_space
                .unmap(gpa, size)
                .map_err(|error| AxVmError::from_addrspace("unmap guest memory region", error))?;
            Ok(())
        })
    }

    /// Reads an object of type `T` from the guest physical address.
    pub fn read_from_guest_of<T>(&self, gpa_ptr: GuestPhysAddr) -> AxVmResult<T> {
        let size = std::mem::size_of::<T>();

        // Ensure the address is properly aligned for the type.
        if !gpa_ptr.as_usize().is_multiple_of(std::mem::align_of::<T>()) {
            return ax_err!(InvalidInput, "Unaligned guest physical address");
        }

        self.with_resources(|resources| {
            let Some(buffers) = resources
                .address_space
                .translated_byte_buffer(gpa_ptr, size)
            else {
                return ax_err!(
                    InvalidInput,
                    "Failed to translate guest physical address or insufficient buffer size"
                );
            };

            let mut data_bytes = Vec::with_capacity(size);
            for chunk in buffers {
                let remaining = size - data_bytes.len();
                let chunk_size = remaining.min(chunk.len());
                data_bytes.extend_from_slice(&chunk[..chunk_size]);
                if data_bytes.len() >= size {
                    break;
                }
            }
            if data_bytes.len() < size {
                return ax_err!(
                    InvalidInput,
                    "Insufficient data in guest memory to read the requested object"
                );
            }
            let data: T = unsafe {
                // Use `ptr::read_unaligned` for safety in case of unaligned memory.
                std::ptr::read_unaligned(data_bytes.as_ptr() as *const T)
            };
            Ok(data)
        })
    }

    /// Reads raw bytes from guest physical memory.
    pub fn read_from_guest(&self, gpa_ptr: GuestPhysAddr, buffer: &mut [u8]) -> AxVmResult {
        self.with_resources(|resources| {
            let Some(chunks) = resources
                .address_space
                .translated_byte_buffer(gpa_ptr, buffer.len())
            else {
                return ax_err!(InvalidInput, "Failed to translate guest physical address");
            };

            let mut copied = 0;
            for chunk in chunks {
                let len = (buffer.len() - copied).min(chunk.len());
                buffer[copied..copied + len].copy_from_slice(&chunk[..len]);
                copied += len;
                if copied == buffer.len() {
                    return Ok(());
                }
            }

            ax_err!(
                InvalidInput,
                "Insufficient guest memory to read the requested buffer"
            )
        })
    }

    /// Writes an object of type `T` to the guest physical address.
    pub fn write_to_guest_of<T>(&self, gpa_ptr: GuestPhysAddr, data: &T) -> AxVmResult {
        let bytes = unsafe {
            std::slice::from_raw_parts(data as *const T as *const u8, std::mem::size_of::<T>())
        };
        self.write_to_guest(gpa_ptr, bytes)
    }

    /// Writes raw bytes into guest physical memory.
    pub fn write_to_guest(&self, gpa_ptr: GuestPhysAddr, data: &[u8]) -> AxVmResult {
        if data.is_empty() {
            return Ok(());
        }

        self.with_resources(|resources| {
            let Some(mut chunks) = resources
                .address_space
                .translated_byte_buffer(gpa_ptr, data.len())
            else {
                return ax_err!(InvalidInput, "Failed to translate guest physical address");
            };

            write_guest_bytes_to_chunks(chunks.as_mut_slice(), data)
        })
    }

    /// Allocates an IVC channel for inter-VM communication region.
    ///
    /// ## Arguments
    /// * `expected_size` - The expected size of the IVC channel in bytes.
    /// ## Returns
    /// * `AxVmResult<(GuestPhysAddr, usize)>` - A tuple containing the guest physical address of the allocated IVC channel and its actual size.
    pub fn alloc_ivc_channel(&self, expected_size: usize) -> AxVmResult<(GuestPhysAddr, usize)> {
        // Ensure the expected size is aligned to 4K.
        let size = align_up_4k(expected_size);
        let gpa = self
            .get_devices()?
            .alloc_ivc_channel(size)
            .map_err(|error| AxVmError::memory("reserve IVC guest address range", error))?;
        Ok((gpa, size))
    }

    /// Releases an IVC channel for inter-VM communication region.
    /// ## Arguments
    /// * `gpa` - The guest physical address of the IVC channel to release.
    /// * `size` - The size of the IVC channel in bytes.
    /// ## Returns
    /// * `AxVmResult<()>` - An empty result indicating success or failure.
    pub fn release_ivc_channel(&self, gpa: GuestPhysAddr, size: usize) -> AxVmResult {
        self.get_devices()?
            .release_ivc_channel(gpa, size)
            .map_err(|error| AxVmError::memory("release IVC guest address range", error))?;
        Ok(())
    }

    /// Allocates a new memory region for the VM.
    pub fn alloc_memory_region(
        &self,
        layout: Layout,
        gpa: Option<GuestPhysAddr>,
    ) -> AxVmResult<&[u8]> {
        assert!(
            layout.size() > 0,
            "Cannot allocate zero-sized memory region"
        );

        let hva = unsafe { std::alloc::alloc_zeroed(layout) };
        if hva.is_null() {
            return Err(AxVmError::OutOfMemory {
                operation: "allocate IVC channel",
            });
        }
        let s = unsafe { std::slice::from_raw_parts_mut(hva, layout.size()) };
        let hva = HostVirtAddr::from_mut_ptr_of(hva);

        let hpa = virt_to_phys(hva);

        let gpa = gpa.unwrap_or_else(|| hpa.as_usize().into());

        if let Err(err) = self.with_resources_mut(|resources| {
            resources
                .address_space
                .map_linear(
                    gpa,
                    hpa,
                    layout.size(),
                    MappingFlags::READ
                        | MappingFlags::WRITE
                        | MappingFlags::EXECUTE
                        | MappingFlags::USER,
                )
                .map_err(|error| AxVmError::from_addrspace("map allocated guest memory", error))?;
            resources.memory_regions.push(VMMemoryRegion {
                gpa,
                hva,
                layout,
                needs_dealloc: true, // This region was allocated and needs to be freed
            });
            Ok(())
        }) {
            unsafe {
                std::alloc::dealloc(hva.as_mut_ptr(), layout);
            }
            return Err(err);
        }

        Ok(s)
    }

    /// Returns a list of all memory regions in the VM.
    pub fn memory_regions(&self) -> Vec<VMMemoryRegion> {
        self.with_resources(|resources| Ok(resources.memory_regions.clone()))
            .unwrap_or_default()
    }

    /// Prepares all memory regions configured for this VM.
    pub fn prepare_memory_layout(&self) -> AxVmResult<PreparedMemoryLayout> {
        let memory_regions =
            self.with_resources(|resources| Ok(resources.config.memory_regions().to_vec()))?;
        let layout = memory::MemoryLayoutBuilder::new(self, &memory_regions).prepare()?;
        let main_memory = layout.main_memory();
        let boot_plan = boot::BootImagePlan::new(main_memory.gpa, main_memory.is_identical());
        self.with_config(|config| boot_plan.apply_to_config(config));
        Ok(layout)
    }

    /// Maps a reserved memory region for the VM.
    pub fn map_reserved_memory_region(
        &self,
        layout: Layout,
        gpa: Option<GuestPhysAddr>,
    ) -> AxVmResult {
        assert!(
            layout.size() > 0,
            "Cannot allocate zero-sized memory region"
        );
        let gpa =
            gpa.ok_or_else(|| ax_err_type!(InvalidInput, "Reserved memory GPA is required"))?;
        self.with_resources_mut(|resources| {
            resources
                .address_space
                .map_linear(
                    gpa,
                    gpa.as_usize().into(),
                    layout.size(),
                    MappingFlags::READ
                        | MappingFlags::WRITE
                        | MappingFlags::EXECUTE
                        | MappingFlags::USER,
                )
                .map_err(|error| AxVmError::from_addrspace("map reserved guest memory", error))?;
            let hva = gpa.as_usize().into();
            resources.memory_regions.push(VMMemoryRegion {
                gpa,
                hva,
                layout,
                needs_dealloc: false, // This is a reserved region, not allocated
            });
            Ok(())
        })
    }

    /// Destroys the VM and releases all lifecycle-owned resources.
    pub fn destroy(&self) -> AxVmResult {
        let vm_id = self.id();
        match self.status() {
            VmStatus::Running | VmStatus::Paused | VmStatus::Stopping => {
                self.stop_and_join_runtime(StopReason::Forced)?;
            }
            VmStatus::Ready | VmStatus::Stopped | VmStatus::Failed => {
                if let Some(runtime) = self.take_stopped_runtime() {
                    runtime.join_all_vcpu_tasks(vm_id)?;
                }
                if self.status() == VmStatus::Stopped {
                    crate::arch::CurrentArch::deactivate_devices(self)?;
                }
            }
            VmStatus::Destroyed | VmStatus::Destroying => {}
            VmStatus::Pausing => {
                self.stop_and_join_runtime(StopReason::Forced)?;
            }
        }
        self.machine.lock().destroy_with(|resources| {
            if let Some(mut resources) = resources {
                Self::cleanup_resource_set(vm_id, &mut resources)?;
            }
            Ok(())
        })
    }

    fn cleanup_resource_set(vm_id: usize, resources: &mut AxVMResources) -> AxVmResult {
        info!("Cleaning up VM[{vm_id}] resources...");

        if let Some(devices) = resources.devices.take() {
            devices.reset_lifecycle_devices().map_err(|error| {
                AxVmError::device("reset device lifecycle during destroy", error)
            })?;
            debug!(
                "VM[{vm_id}] devices cleanup: {} device(s)",
                devices.devices().count()
            );
        }

        let regions_to_cleanup = resources.memory_regions.clone();
        for region in &regions_to_cleanup {
            debug!(
                "VM[{vm_id}] unmapping memory region: GPA={:#x}, size={:#x}",
                region.gpa.as_usize(),
                region.size()
            );
            if let Err(err) = resources.address_space.unmap(region.gpa, region.size()) {
                warn!(
                    "VM[{vm_id}] failed to unmap region at GPA={:#x}: {err:?}",
                    region.gpa.as_usize()
                );
            }
        }

        for region in &regions_to_cleanup {
            if region.needs_dealloc {
                debug!(
                    "VM[{vm_id}] deallocating memory region: HVA={:#x}, size={:#x}",
                    region.hva.as_usize(),
                    region.size()
                );
                unsafe {
                    std::alloc::dealloc(region.hva.as_mut_ptr(), region.layout);
                }
            } else {
                debug!(
                    "VM[{vm_id}] skipping reserved memory region dealloc: GPA={:#x}, HVA={:#x}, \
                     size={:#x}",
                    region.gpa.as_usize(),
                    region.hva.as_usize(),
                    region.size()
                );
            }
        }
        resources.memory_regions.clear();
        resources.address_space.clear();

        resources.vcpu_list = None;
        resources.interrupt_controller = None;

        info!("VM[{vm_id}] resources cleanup completed");
        Ok(())
    }
}

impl Drop for AxVM {
    fn drop(&mut self) {
        info!("Dropping VM[{}]", self.id());

        if let Err(err) = self.destroy() {
            warn!("VM[{}] destroy during drop failed: {err:?}", self.id());
        }

        info!("VM[{}] dropped", self.id());
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, sync::atomic::AtomicBool};

    use axdevice_base::{
        ControllerInputId, InterruptControllerId, InterruptEndpoint, InterruptTriggerMode,
        IrqError, IrqResult, VirtualInterruptController, WiredIrqInput, WiredIrqSink,
    };

    use super::*;

    #[test]
    fn write_guest_bytes_to_chunks_writes_only_remaining_bytes() {
        let mut first = [0u8; 2];
        let mut second = [0u8; 4];
        let mut chunks: [&mut [u8]; 2] = [&mut first, &mut second];

        write_guest_bytes_to_chunks(&mut chunks, &[1, 2, 3]).unwrap();

        assert_eq!(first, [1, 2]);
        assert_eq!(second, [3, 0, 0, 0]);
    }

    #[test]
    fn write_guest_bytes_to_chunks_rejects_insufficient_capacity() {
        let mut only = [0u8; 2];
        let mut chunks: [&mut [u8]; 1] = [&mut only];

        let err = write_guest_bytes_to_chunks(&mut chunks, &[1, 2, 3]).unwrap_err();

        assert!(matches!(err, AxVmError::InvalidInput { .. }));
        assert_eq!(only, [1, 2]);
    }

    #[test]
    fn write_guest_bytes_to_chunks_accepts_empty_writes() {
        let mut chunk = [7u8; 2];
        let mut chunks: [&mut [u8]; 1] = [&mut chunk];

        write_guest_bytes_to_chunks(&mut chunks, &[]).unwrap();

        assert_eq!(chunk, [7, 7]);
    }

    fn drain_normal_vectors(runtime: &VmRuntimeHandle, vcpu_id: usize) -> Vec<usize> {
        runtime
            .drain_pending_interrupts(vcpu_id)
            .into_iter()
            .map(|interrupt| match interrupt {
                PendingInterrupt::Normal(vector) => vector,
                PendingInterrupt::External { .. } => {
                    panic!("unexpected external interrupt in normal queue test")
                }
            })
            .collect()
    }

    #[test]
    fn runtime_pending_interrupts_are_per_vcpu_and_drained_once() {
        let runtime = VmRuntimeHandle::new();

        assert_eq!(
            runtime
                .queue_pending_interrupt_for_cpu(0, 3, PendingInterrupt::Normal(2))
                .unwrap(),
            3
        );
        assert_eq!(
            runtime
                .queue_pending_interrupt_for_cpu(1, 5, PendingInterrupt::Normal(9))
                .unwrap(),
            5
        );

        assert_eq!(drain_normal_vectors(&runtime, 0), std::vec![2]);
        assert_eq!(drain_normal_vectors(&runtime, 1), std::vec![9]);

        assert!(drain_normal_vectors(&runtime, 0).is_empty());
        assert!(drain_normal_vectors(&runtime, 1).is_empty());
    }

    #[test]
    fn runtime_dispatch_orders_enqueue_before_notify_and_ipi() {
        let events = RefCell::new(Vec::new());

        dispatch_vcpu_interrupt_with(
            || {
                events.borrow_mut().push("enqueue");
                Ok(3)
            },
            || events.borrow_mut().push("notify"),
            |cpu_id| {
                assert_eq!(cpu_id, 3);
                events.borrow_mut().push("ipi");
            },
        )
        .unwrap();

        assert_eq!(*events.borrow(), ["enqueue", "notify", "ipi"]);
    }

    #[cfg(feature = "host-test")]
    #[test]
    fn runtime_dispatch_releases_queue_lock_before_callbacks() {
        let dispatcher = VcpuIrqDispatcher::new();
        dispatcher.register_test_vcpu(0, 3);
        let interrupt = PendingVcpuInterrupt {
            id: crate::irq::model::VirtualInterruptId(7),
            trigger: crate::InterruptTriggerMode::LevelTriggered,
        };
        let events = RefCell::new(Vec::new());

        dispatch_vcpu_interrupt_with(
            || dispatcher.enqueue(0, interrupt),
            || {
                assert_eq!(dispatcher.drain(0), std::vec![interrupt]);
                events.borrow_mut().push("notify");
            },
            |_| events.borrow_mut().push("ipi"),
        )
        .unwrap();

        assert_eq!(*events.borrow(), ["notify", "ipi"]);
    }

    #[test]
    fn runtime_dispatch_stops_when_enqueue_fails() {
        let events = RefCell::new(Vec::new());

        let result = dispatch_vcpu_interrupt_with(
            || {
                events.borrow_mut().push("enqueue");
                Err(ax_err_type!(NotFound, "vCPU task not found"))
            },
            || events.borrow_mut().push("notify"),
            |_| events.borrow_mut().push("ipi"),
        );

        assert!(matches!(result, Err(AxVmError::ResourceUnavailable { .. })));
        assert_eq!(*events.borrow(), ["enqueue"]);
    }

    #[test]
    fn runtime_records_the_first_lifecycle_error_once() {
        let runtime = VmRuntimeHandle::new();
        let first = AxVmError::interrupt("deactivate architecture devices", "first failure");
        let second = AxVmError::device("finish VM stop", "second failure");

        runtime.record_lifecycle_error(first.clone());
        runtime.record_lifecycle_error(second);

        assert_eq!(runtime.take_lifecycle_error(), Some(first));
        assert_eq!(runtime.take_lifecycle_error(), None);
    }

    #[test]
    fn interrupt_pulse_runs_after_snapshot_lock_is_released() {
        let machine_lock = Arc::new(TestMachineLock::default());
        let sink = Arc::new(TestIrqSink::with_machine_lock(machine_lock.clone()));
        let controller: Arc<dyn VirtualInterruptController> =
            Arc::new(TestInterruptController { sink: sink.clone() });

        pulse_interrupt_with_snapshot(
            || {
                let _machine = machine_lock.lock();
                Ok(controller.clone())
            },
            7,
        )
        .unwrap();

        assert_eq!(sink.pulse_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn interrupt_snapshot_failure_does_not_call_sink() {
        let sink = Arc::new(TestIrqSink::default());
        let expected = ax_err_type!(BadState, "interrupt snapshot unavailable");

        let result = pulse_interrupt_with_snapshot(|| Err(expected.clone()), 9);

        assert_eq!(result, Err(expected));
        assert_eq!(sink.pulse_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn interrupt_pulse_propagates_sink_error() {
        let irq_error = IrqError::Backend {
            endpoint: InterruptEndpoint::Wired {
                controller: InterruptControllerId::new(0),
                input: ControllerInputId::new(11),
            },
            operation: "test pulse",
            detail: "controller rejected interrupt".into(),
        };
        let sink = Arc::new(TestIrqSink::with_pulse_error(irq_error.clone()));
        let controller: Arc<dyn VirtualInterruptController> =
            Arc::new(TestInterruptController { sink });

        let result = pulse_interrupt_with_snapshot(|| Ok(controller.clone()), 11);

        assert_eq!(result, Err(AxVmError::from(irq_error)));
    }

    #[derive(Default)]
    struct TestIrqSink {
        machine_lock: Option<Arc<TestMachineLock>>,
        pulse_error: Option<IrqError>,
        pulse_count: AtomicUsize,
    }

    impl TestIrqSink {
        fn with_machine_lock(machine_lock: Arc<TestMachineLock>) -> Self {
            Self {
                machine_lock: Some(machine_lock),
                ..Self::default()
            }
        }

        fn with_pulse_error(pulse_error: IrqError) -> Self {
            Self {
                pulse_error: Some(pulse_error),
                ..Self::default()
            }
        }
    }

    struct TestInterruptController {
        sink: Arc<TestIrqSink>,
    }

    impl VirtualInterruptController for TestInterruptController {
        fn id(&self) -> InterruptControllerId {
            InterruptControllerId::new(0)
        }

        fn wired_input(
            &self,
            input: ControllerInputId,
            trigger: InterruptTriggerMode,
        ) -> IrqResult<WiredIrqInput> {
            let sink: Arc<dyn WiredIrqSink> = self.sink.clone();
            Ok(WiredIrqInput::new(self.id(), input, trigger, sink))
        }
    }

    impl WiredIrqSink for TestIrqSink {
        fn set_level(&self, _input: ControllerInputId, _asserted: bool) -> IrqResult {
            Ok(())
        }

        fn pulse(&self, _input: ControllerInputId) -> IrqResult {
            let _machine = self.machine_lock.as_ref().map(|machine_lock| {
                machine_lock
                    .try_lock()
                    .expect("interrupt callback must run without the machine lock")
            });
            self.pulse_count.fetch_add(1, Ordering::Relaxed);
            self.pulse_error.clone().map_or(Ok(()), Err)
        }
    }

    #[derive(Default)]
    struct TestMachineLock {
        held: AtomicBool,
    }

    impl TestMachineLock {
        fn lock(&self) -> TestMachineGuard<'_> {
            self.try_lock().expect("test machine lock is already held")
        }

        fn try_lock(&self) -> Option<TestMachineGuard<'_>> {
            self.held
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .ok()
                .map(|_| TestMachineGuard { lock: self })
        }
    }

    struct TestMachineGuard<'a> {
        lock: &'a TestMachineLock,
    }

    impl Drop for TestMachineGuard<'_> {
        fn drop(&mut self) {
            self.lock.held.store(false, Ordering::Release);
        }
    }
}

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

use alloc::{boxed::Box, collections::BTreeMap, format, string::String, sync::Arc, vec::Vec};
use core::{
    alloc::Layout,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_cpumask::CpuMask;
use ax_errno::{AxError, AxResult, ax_err, ax_err_type};
use ax_kspin::SpinNoIrq as Mutex;
use ax_memory_addr::align_up_4k;
use axaddrspace::AddrSpace;
use axdevice::{AxVmDevices, FwCfg, FwCfgPlatformConfig};
use axdevice_base::AccessWidth;
use axvm_types::{
    GuestPhysAddr, HostPhysAddr, HostVirtAddr, MappingFlags, NestedPagingConfig, VmVcpuState,
};

use crate::{
    arch::ArchNestedPageTable,
    boot::{GuestBootDescription, GuestFdtBuilder},
    config::{AxVMConfig, PhysCpuList, VMInterruptMode},
    host::paging::virt_to_phys,
    irq::InterruptFabric,
    layout::VmAddressLayout,
    lifecycle::{Machine, StopReason, VmLifecycleError, VmStatus},
    vcpu::AxVCpu,
};

pub(crate) mod boot;
pub(crate) mod memory;
mod prepare;
pub use memory::PreparedMemoryLayout;

const VM_ASPACE_BASE: usize = 0x0;
const VM_ASPACE_SIZE: usize = 0x7fff_ffff_f000;

/// A vCPU with architecture-independent interface.
type VCpu = AxVCpu<crate::arch::ArchVCpu>;
/// A reference to a vCPU.
pub(crate) type AxVCpuRef<A = crate::arch::ArchVCpu> = Arc<AxVCpu<A>>;
/// A reference to a VM.
pub type AxVMRef = Arc<AxVM>;

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

fn write_guest_bytes_to_chunks(chunks: &mut [&mut [u8]], data: &[u8]) -> AxResult {
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
    address_space: AddrSpace<ArchNestedPageTable>,
    nested_paging: NestedPagingConfig,
    memory_regions: Vec<VMMemoryRegion>,
    config: AxVMConfig,
    phys_cpu_ls: PhysCpuList,
    vcpu_list: Option<Box<[AxVCpuRef]>>,
    devices: Option<Arc<AxVmDevices>>,
    interrupt_fabric: Option<InterruptFabric>,
    address_layout: Option<VmAddressLayout>,
    boot_description: GuestBootDescription,
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
    pending_interrupts: Mutex<BTreeMap<usize, Vec<PendingInterrupt>>>,
    running_halting_vcpu_count: AtomicUsize,
}

impl VmRuntimeHandle {
    pub(crate) fn new() -> Self {
        Self {
            wait_queue: crate::WaitQueue::new(),
            vcpu_task_list: Mutex::new(BTreeMap::new()),
            pending_interrupts: Mutex::new(BTreeMap::new()),
            running_halting_vcpu_count: AtomicUsize::new(0),
        }
    }

    pub(crate) fn add_vcpu_task(&self, vcpu_id: usize, vcpu_task: crate::AxTaskRef) {
        self.vcpu_task_list.lock().insert(vcpu_id, vcpu_task);
        self.pending_interrupts.lock().entry(vcpu_id).or_default();
    }

    pub(crate) fn queue_interrupt(&self, vcpu_id: usize, vector: usize) -> AxResult<usize> {
        let task = self
            .vcpu_task_list
            .lock()
            .get(&vcpu_id)
            .cloned()
            .ok_or_else(|| ax_err_type!(NotFound, format!("vCPU {vcpu_id} task not found")))?;
        self.pending_interrupts
            .lock()
            .entry(vcpu_id)
            .or_default()
            .push(PendingInterrupt::Normal(vector));
        Ok(task.cpu_id() as usize)
    }

    #[expect(
        dead_code,
        reason = "only the LoongArch IRQ backend queues physical interrupts"
    )]
    pub(crate) fn queue_external_interrupt(
        &self,
        vcpu_id: usize,
        vector: usize,
        physical_irq: usize,
    ) -> AxResult<usize> {
        let task = self
            .vcpu_task_list
            .lock()
            .get(&vcpu_id)
            .cloned()
            .ok_or_else(|| ax_err_type!(NotFound, format!("vCPU {vcpu_id} task not found")))?;
        self.pending_interrupts
            .lock()
            .entry(vcpu_id)
            .or_default()
            .push(PendingInterrupt::External {
                vector,
                physical_irq,
            });
        Ok(task.cpu_id() as usize)
    }

    pub(crate) fn drain_pending_interrupts(&self, vcpu_id: usize) -> Vec<PendingInterrupt> {
        self.pending_interrupts
            .lock()
            .get_mut(&vcpu_id)
            .map(core::mem::take)
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

    pub(crate) fn mark_vcpu_exiting(&self) -> bool {
        self.running_halting_vcpu_count.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |count| count.checked_sub(1),
        ) == Ok(1)
    }

    pub(crate) fn join_all_vcpu_tasks(&self, vm_id: usize) {
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
    }
}

impl AxVMResources {
    fn new(config: AxVMConfig) -> AxResult<Self> {
        let vcpu_mappings = config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        let page_table_levels = crate::arch::guest_page_table_levels(&vcpu_mappings)?;
        let page_table = crate::arch::new_nested_page_table(page_table_levels)?;
        let address_space = AddrSpace::new_empty(
            page_table,
            GuestPhysAddr::from(VM_ASPACE_BASE),
            VM_ASPACE_SIZE,
        )?;
        let nested_paging = crate::arch::nested_paging_config(
            address_space.page_table_root(),
            page_table_levels,
            &vcpu_mappings,
        )?;
        Ok(Self {
            address_space,
            nested_paging,
            memory_regions: Vec::new(),
            config,
            phys_cpu_ls: PhysCpuList::default(),
            vcpu_list: None,
            devices: None,
            interrupt_fabric: None,
            address_layout: None,
            boot_description: GuestBootDescription::none(),
        })
    }

    fn vcpu_list(&self) -> AxResult<&[AxVCpuRef]> {
        self.vcpu_list
            .as_deref()
            .ok_or_else(|| ax_err_type!(BadState, "VM vCPU resources are not prepared"))
    }

    fn devices(&self) -> AxResult<Arc<AxVmDevices>> {
        self.devices
            .clone()
            .ok_or_else(|| ax_err_type!(BadState, "VM devices are not prepared"))
    }

    fn interrupt_fabric(&self) -> AxResult<&InterruptFabric> {
        self.interrupt_fabric
            .as_ref()
            .ok_or_else(|| ax_err_type!(BadState, "VM interrupt fabric is not prepared"))
    }

    fn reset_transient_resources(&mut self) -> AxResult {
        let memory_regions = self.memory_regions.clone();
        self.address_space.clear();
        for region in &memory_regions {
            self.address_space.map_linear(
                region.gpa,
                region.host_paddr(),
                region.size(),
                MappingFlags::READ
                    | MappingFlags::WRITE
                    | MappingFlags::EXECUTE
                    | MappingFlags::USER,
            )?;
        }
        self.vcpu_list = None;
        self.devices = None;
        self.interrupt_fabric = None;
        self.address_layout = None;
        Ok(())
    }
}

struct PendingFwCfg {
    base: GuestPhysAddr,
    size: usize,
    kernel: &'static [u8],
    initrd: Option<&'static [u8]>,
    cmdline: Option<String>,
    cpu_num: u16,
    platform: FwCfgPlatformConfig,
}

pub struct FwCfgDeviceConfig {
    pub base: GuestPhysAddr,
    pub size: usize,
    pub kernel: &'static [u8],
    pub initrd: Option<&'static [u8]>,
    pub cmdline: Option<String>,
    pub cpu_num: u16,
    pub platform: FwCfgPlatformConfig,
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
    pending_fw_cfg: Mutex<Option<PendingFwCfg>>,
}

impl AxVM {
    /// Creates a new VM with the given configuration.
    /// Returns an error if the configuration is invalid.
    /// The VM is not started until `start` is called.
    pub fn new(config: AxVMConfig) -> AxResult<AxVMRef> {
        let id = config.id();
        let name = config.name();
        let result = Arc::new(Self {
            id,
            name,
            machine: Mutex::new(Machine::Uninit(Box::new(config))),
            pending_fw_cfg: Mutex::new(None),
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

    /// Returns the configured VM interrupt mode.
    pub fn interrupt_mode(&self) -> VMInterruptMode {
        let _ = self.ensure_resources_ready();
        self.with_resources(|resources| Ok(resources.config.interrupt_mode()))
            .unwrap_or(VMInterruptMode::NoIrq)
    }

    fn ensure_resources_ready(&self) -> AxResult {
        let mut machine = self.machine.lock();
        if !matches!(machine.status(), VmStatus::Uninit) {
            return Ok(());
        }
        machine
            .prepare_with(|config| {
                AxVMResources::new(config).map_err(|err| {
                    VmLifecycleError::MissingResource(match err {
                        AxError::NoMemory => "VM address space memory",
                        _ => "VM address space",
                    })
                })
            })
            .map_err(VmLifecycleError::into_ax_error)
    }

    fn with_resources<F, R>(&self, f: F) -> AxResult<R>
    where
        F: FnOnce(&AxVMResources) -> AxResult<R>,
    {
        self.ensure_resources_ready()?;
        let machine = self.machine.lock();
        let resources = machine
            .resources()
            .ok_or_else(|| ax_err_type!(BadState, "VM resources are not available"))?;
        f(resources)
    }

    fn with_resources_mut<F, R>(&self, f: F) -> AxResult<R>
    where
        F: FnOnce(&mut AxVMResources) -> AxResult<R>,
    {
        self.ensure_resources_ready()?;
        let mut machine = self.machine.lock();
        let resources = machine
            .resources_mut()
            .ok_or_else(|| ax_err_type!(BadState, "VM resources are not available"))?;
        f(resources)
    }

    pub(crate) fn with_runtime<F, R>(&self, f: F) -> AxResult<R>
    where
        F: FnOnce(&Arc<VmRuntimeHandle>) -> AxResult<R>,
    {
        let machine = self.machine.lock();
        let runtime = machine
            .runtime()
            .ok_or_else(|| ax_err_type!(BadState, "VM runtime is not available"))?;
        f(runtime)
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
    pub fn nested_page_table_root(&self) -> AxResult<HostPhysAddr> {
        self.with_resources(|resources| Ok(resources.address_space.page_table_root()))
    }

    /// Returns to the VM's configuration.
    pub fn with_config<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut AxVMConfig) -> R,
    {
        let mut machine = self.machine.lock();
        if matches!(machine.status(), VmStatus::Uninit) {
            let old = core::mem::replace(&mut *machine, Machine::Switching);
            if let Machine::Uninit(config) = old {
                match AxVMResources::new(*config) {
                    Ok(resources) => *machine = Machine::Ready(resources),
                    Err(err) => {
                        *machine = Machine::Failed(format!("prepare config resources: {err:?}"));
                        panic!("VM resources are not available: {err:?}");
                    }
                }
            } else {
                *machine = old;
            }
        }
        let resources = machine
            .resources_mut()
            .expect("VM resources are not available for config access");
        f(&mut resources.config)
    }

    /// Stores a guest DTB as VM-owned boot-description state.
    pub fn set_guest_device_tree(&self, load_gpa: GuestPhysAddr, bytes: Vec<u8>) -> AxResult {
        self.with_resources_mut(|resources| {
            resources.config.set_dtb_load_gpa(load_gpa);
            resources
                .boot_description
                .set_device_tree(GuestFdtBuilder::from_bytes(bytes).build(load_gpa));
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
    ) -> AxResult<Vec<&'static mut [u8]>> {
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
    pub fn start(self: &Arc<Self>) -> AxResult {
        self.ensure_resources_ready()?;
        if self.status() == VmStatus::Stopped {
            if let Some(runtime) = self.take_stopped_runtime() {
                runtime.join_all_vcpu_tasks(self.id());
            }
            self.prepare()?;
        }
        info!("Starting VM[{}]", self.id());
        let primary_vcpu = self
            .vcpu(0)
            .ok_or_else(|| ax_err_type!(BadState, "VM primary vCPU is not prepared"))?;
        let primary_task = crate::runtime::vcpus::build_vcpu_task(self, primary_vcpu);
        let runtime = Arc::new(VmRuntimeHandle::new());

        self.machine
            .lock()
            .start_with(|resources| {
                resources
                    .vcpu_list()
                    .map_err(|_| VmLifecycleError::MissingResource("vCPU list"))?;
                resources
                    .devices()
                    .map_err(|_| VmLifecycleError::MissingResource("devices"))?;
                resources
                    .interrupt_fabric()
                    .map_err(|_| VmLifecycleError::MissingResource("interrupt fabric"))?;
                Ok(runtime.clone())
            })
            .map_err(VmLifecycleError::into_ax_error)?;

        let task = crate::host::task::spawn_task(primary_task);
        runtime.add_vcpu_task(0, task);
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
    pub fn pause(&self) -> AxResult {
        self.machine
            .lock()
            .pause()
            .map_err(VmLifecycleError::into_ax_error)
    }

    /// Resumes a paused VM.
    pub fn resume(&self) -> AxResult {
        self.machine
            .lock()
            .resume()
            .map_err(VmLifecycleError::into_ax_error)
    }

    /// Requests a stop. Running vCPUs observe the Stopping state and exit.
    pub fn stop(&self, reason: StopReason) -> AxResult {
        info!("Stopping VM[{}]: {reason:?}", self.id());
        self.machine
            .lock()
            .request_stop_with(reason, |_, _| Ok(()))
            .map_err(VmLifecycleError::into_ax_error)
    }

    pub(crate) fn finish_stop(&self) -> AxResult {
        self.machine
            .lock()
            .finish_stop()
            .map_err(VmLifecycleError::into_ax_error)
    }

    fn wait_until_stopped(&self) -> AxResult {
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

    fn stop_and_join_runtime(&self, reason: StopReason) -> AxResult {
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
            runtime.join_all_vcpu_tasks(self.id());
        }
        Ok(())
    }

    /// Resets the VM by discarding runtime-only state, rebuilding vCPUs/devices,
    /// and starting from a fresh `Running` state.
    pub fn reset(self: &Arc<Self>) -> AxResult {
        info!("Resetting VM[{}]", self.id());
        self.stop_and_join_runtime(StopReason::Forced)?;

        self.machine
            .lock()
            .reset_with(|resources| {
                resources
                    .reset_transient_resources()
                    .map_err(|_| VmLifecycleError::MissingResource("reset resources"))
            })
            .map_err(VmLifecycleError::into_ax_error)?;
        self.prepare()?;
        self.start()
    }

    /// Returns this VM's emulated devices.
    pub fn get_devices(&self) -> AxResult<Arc<AxVmDevices>> {
        self.with_resources(|resources| resources.devices())
    }

    /// Pulses a prepared VM interrupt fabric line without exposing the fabric.
    pub fn pulse_interrupt(&self, irq_id: usize) -> AxResult {
        match self.status() {
            VmStatus::Running | VmStatus::Paused => {
                self.with_resources(|resources| resources.interrupt_fabric()?.pulse(irq_id))
            }
            status => ax_err!(
                BadState,
                format!("VM[{}] cannot accept IRQ in {status:?}", self.id())
            ),
        }
    }

    /// Returns the number of prepared emulated devices.
    pub fn device_count(&self) -> usize {
        self.get_devices()
            .map(|devices| devices.devices().count())
            .unwrap_or(0)
    }

    /// Queue a QEMU fw_cfg device that will be attached during VM initialization.
    pub fn add_fw_cfg_device(&self, config: FwCfgDeviceConfig) -> AxResult {
        let mut pending = self.pending_fw_cfg.lock();
        if pending.is_some() {
            return ax_err!(
                AlreadyExists,
                format!("VM[{}] fw_cfg device already exists", self.id())
            );
        }
        *pending = Some(PendingFwCfg {
            base: config.base,
            size: config.size,
            kernel: config.kernel,
            initrd: config.initrd,
            cmdline: config.cmdline,
            cpu_num: config.cpu_num,
            platform: config.platform,
        });
        debug!(
            "VM[{}] queued fw_cfg device: base={:#x}, size={:#x}, kernel={} bytes, initrd={:?}",
            self.id(),
            config.base.as_usize(),
            config.size,
            config.kernel.len(),
            config.initrd.map(|data| data.len())
        );
        Ok(())
    }

    fn add_special_emulated_devices(&self, devices: &mut AxVmDevices) -> AxResult {
        if let Some(pending) = self.pending_fw_cfg.lock().take() {
            debug!(
                "VM[{}] adding fw_cfg MMIO device at [{:#x},{:#x})",
                self.id(),
                pending.base.as_usize(),
                pending.base.as_usize() + pending.size
            );
            devices.add_fw_cfg_dev(Arc::new(FwCfg::new(
                pending.base,
                pending.size,
                pending.kernel,
                pending.initrd,
                pending.cmdline.as_deref(),
                pending.cpu_num,
                pending.platform,
            )))?;
        }
        Ok(())
    }

    pub(crate) fn handle_mmio_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        data: usize,
    ) -> AxResult {
        let devices = self.get_devices()?;
        if let Some(fw_cfg) = devices.fw_cfg_for_dma_addr(addr) {
            if let Some(desc_addr) = fw_cfg.write_dma_address(addr, width, data)? {
                fw_cfg.process_dma(
                    desc_addr,
                    |gpa, buffer| self.read_from_guest(gpa, buffer),
                    |gpa, buffer| self.write_to_guest(gpa, buffer),
                )?;
            }
            return Ok(());
        }

        devices.handle_mmio_write(addr, width, data)?;
        Ok(())
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
        match resources.address_space.page_table().query(addr) {
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
    ) -> AxResult {
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

    /// Maps a region of host physical memory to guest physical memory.
    pub fn map_region(
        &self,
        gpa: GuestPhysAddr,
        hpa: HostPhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> AxResult {
        self.with_resources_mut(|resources| {
            resources.address_space.map_linear(gpa, hpa, size, flags)?;
            Ok(())
        })
    }

    /// Unmaps a region of guest physical memory.
    pub fn unmap_region(&self, gpa: GuestPhysAddr, size: usize) -> AxResult {
        self.with_resources_mut(|resources| {
            resources.address_space.unmap(gpa, size)?;
            Ok(())
        })
    }

    /// Reads an object of type `T` from the guest physical address.
    pub fn read_from_guest_of<T>(&self, gpa_ptr: GuestPhysAddr) -> AxResult<T> {
        let size = core::mem::size_of::<T>();

        // Ensure the address is properly aligned for the type.
        if !gpa_ptr
            .as_usize()
            .is_multiple_of(core::mem::align_of::<T>())
        {
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
                core::ptr::read_unaligned(data_bytes.as_ptr() as *const T)
            };
            Ok(data)
        })
    }

    /// Reads raw bytes from guest physical memory.
    pub fn read_from_guest(&self, gpa_ptr: GuestPhysAddr, buffer: &mut [u8]) -> AxResult {
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
    pub fn write_to_guest_of<T>(&self, gpa_ptr: GuestPhysAddr, data: &T) -> AxResult {
        let bytes = unsafe {
            core::slice::from_raw_parts(data as *const T as *const u8, core::mem::size_of::<T>())
        };
        self.write_to_guest(gpa_ptr, bytes)
    }

    /// Writes raw bytes into guest physical memory.
    pub fn write_to_guest(&self, gpa_ptr: GuestPhysAddr, data: &[u8]) -> AxResult {
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
    /// * `AxResult<(GuestPhysAddr, usize)>` - A tuple containing the guest physical address of the allocated IVC channel and its actual size.
    pub fn alloc_ivc_channel(&self, expected_size: usize) -> AxResult<(GuestPhysAddr, usize)> {
        // Ensure the expected size is aligned to 4K.
        let size = align_up_4k(expected_size);
        let gpa = self.get_devices()?.alloc_ivc_channel(size)?;
        Ok((gpa, size))
    }

    /// Releases an IVC channel for inter-VM communication region.
    /// ## Arguments
    /// * `gpa` - The guest physical address of the IVC channel to release.
    /// * `size` - The size of the IVC channel in bytes.
    /// ## Returns
    /// * `AxResult<()>` - An empty result indicating success or failure.
    pub fn release_ivc_channel(&self, gpa: GuestPhysAddr, size: usize) -> AxResult {
        self.get_devices()?.release_ivc_channel(gpa, size)?;
        Ok(())
    }

    /// Allocates a new memory region for the VM.
    pub fn alloc_memory_region(
        &self,
        layout: Layout,
        gpa: Option<GuestPhysAddr>,
    ) -> AxResult<&[u8]> {
        assert!(
            layout.size() > 0,
            "Cannot allocate zero-sized memory region"
        );

        let hva = unsafe { alloc::alloc::alloc_zeroed(layout) };
        if hva.is_null() {
            return Err(AxError::NoMemory);
        }
        let s = unsafe { core::slice::from_raw_parts_mut(hva, layout.size()) };
        let hva = HostVirtAddr::from_mut_ptr_of(hva);

        let hpa = virt_to_phys(hva);

        let gpa = gpa.unwrap_or_else(|| hpa.as_usize().into());

        if let Err(err) = self.with_resources_mut(|resources| {
            resources.address_space.map_linear(
                gpa,
                hpa,
                layout.size(),
                MappingFlags::READ
                    | MappingFlags::WRITE
                    | MappingFlags::EXECUTE
                    | MappingFlags::USER,
            )?;
            resources.memory_regions.push(VMMemoryRegion {
                gpa,
                hva,
                layout,
                needs_dealloc: true, // This region was allocated and needs to be freed
            });
            Ok(())
        }) {
            unsafe {
                alloc::alloc::dealloc(hva.as_mut_ptr(), layout);
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
    pub fn prepare_memory_layout(&self) -> AxResult<PreparedMemoryLayout> {
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
    ) -> AxResult {
        assert!(
            layout.size() > 0,
            "Cannot allocate zero-sized memory region"
        );
        let gpa =
            gpa.ok_or_else(|| ax_err_type!(InvalidInput, "Reserved memory GPA is required"))?;
        self.with_resources_mut(|resources| {
            resources.address_space.map_linear(
                gpa,
                gpa.as_usize().into(),
                layout.size(),
                MappingFlags::READ
                    | MappingFlags::WRITE
                    | MappingFlags::EXECUTE
                    | MappingFlags::USER,
            )?;
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
    pub fn destroy(&self) -> AxResult {
        let vm_id = self.id();
        match self.status() {
            VmStatus::Running | VmStatus::Paused | VmStatus::Stopping => {
                self.stop_and_join_runtime(StopReason::Forced)?;
            }
            VmStatus::Ready | VmStatus::Stopped | VmStatus::Uninit | VmStatus::Failed => {
                if let Some(runtime) = self.take_stopped_runtime() {
                    runtime.join_all_vcpu_tasks(vm_id);
                }
            }
            VmStatus::Destroyed | VmStatus::Destroying => {}
            VmStatus::Pausing => {
                self.stop_and_join_runtime(StopReason::Forced)?;
            }
        }
        self.machine
            .lock()
            .destroy_with(|resources| {
                if let Some(mut resources) = resources {
                    Self::cleanup_resource_set(vm_id, &mut resources);
                }
                Ok(())
            })
            .map_err(VmLifecycleError::into_ax_error)
    }

    fn cleanup_resource_set(vm_id: usize, resources: &mut AxVMResources) {
        info!("Cleaning up VM[{vm_id}] resources...");

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
                    alloc::alloc::dealloc(region.hva.as_mut_ptr(), region.layout);
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

        if let Some(devices) = resources.devices.take() {
            debug!(
                "VM[{vm_id}] devices cleanup: {} device(s)",
                devices.devices().count()
            );
        }
        resources.vcpu_list = None;
        resources.interrupt_fabric = None;

        info!("VM[{vm_id}] resources cleanup completed");
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

        assert_eq!(err, AxError::InvalidInput);
        assert_eq!(only, [1, 2]);
    }

    #[test]
    fn write_guest_bytes_to_chunks_accepts_empty_writes() {
        let mut chunk = [7u8; 2];
        let mut chunks: [&mut [u8]; 1] = [&mut chunk];

        write_guest_bytes_to_chunks(&mut chunks, &[]).unwrap();

        assert_eq!(chunk, [7, 7]);
    }
}

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

use alloc::{boxed::Box, format, string::String, sync::Arc, vec::Vec};
use core::{alloc::Layout, fmt};

use ax_cpumask::CpuMask;
use ax_errno::{AxError, AxResult, ax_err, ax_err_type};
use ax_kspin::SpinNoIrq as Mutex;
use ax_memory_addr::{align_down_4k, align_up_4k};
use axaddrspace::{AddrSpace, MappingFlags};
use axdevice::{
    AxVmDeviceConfig, AxVmDevices, DeviceBuildContext, DeviceFactoryRegistry, FwCfg,
    FwCfgPlatformConfig, register_builtin_factories,
};
use axdevice_base::AccessWidth;
#[cfg(target_arch = "aarch64")]
use axdevice_base::DeviceRegistry as _;
#[cfg(target_arch = "x86_64")]
use axdevice_base::DeviceRegistry as _;
#[cfg(target_arch = "x86_64")]
use axdevice_base::{BaseDeviceOps, PortDeviceAdapter};
use axvcpu::{AxArchVCpu, AxVCpu, AxVCpuExitReason, VCpuState};
#[cfg(target_arch = "x86_64")]
use axvm_types::EmulatedDeviceType;
use axvm_types::{GuestPhysAddr, HostPhysAddr, HostVirtAddr};
use spin::Once;
#[cfg(all(target_arch = "x86_64", feature = "vmx"))]
use x86_vcpu::{X86_APIC_ACCESS_GPA, x86_apic_access_page_addr};

#[cfg(not(target_arch = "x86_64"))]
use crate::vcpu::AxVCpuCreateConfig;
use crate::{
    config::{AxVMConfig, PhysCpuList, VMInterruptMode},
    host::paging::{HostPagingHandler, virt_to_phys},
    irq::InterruptFabric,
    vcpu::AxArchVCpuImpl,
};

const VM_ASPACE_BASE: usize = 0x0;
const VM_ASPACE_SIZE: usize = 0x7fff_ffff_f000;

/// A vCPU with architecture-independent interface.
type VCpu = AxVCpu<AxArchVCpuImpl>;
/// A reference to a vCPU.
pub type AxVCpuRef = Arc<VCpu>;
/// A reference to a VM.
pub type AxVMRef = Arc<AxVM>;

fn width_mask(width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => 0xff,
        AccessWidth::Word => 0xffff,
        AccessWidth::Dword => 0xffff_ffff,
        AccessWidth::Qword => usize::MAX,
    }
}

fn sign_extend_value(value: usize, width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => (value as i8) as isize as usize,
        AccessWidth::Word => (value as i16) as isize as usize,
        AccessWidth::Dword => (value as i32) as isize as usize,
        AccessWidth::Qword => value,
    }
}

struct AxVMInnerConst {
    phys_cpu_ls: PhysCpuList,
    vcpu_list: Box<[AxVCpuRef]>,
    devices: AxVmDevices,
    interrupt_fabric: InterruptFabric,
}

unsafe impl Send for AxVMInnerConst {}
unsafe impl Sync for AxVMInnerConst {}

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

struct AxVMInnerMut {
    // Todo: use more efficient lock.
    address_space: AddrSpace<HostPagingHandler>,
    memory_regions: Vec<VMMemoryRegion>,
    config: AxVMConfig,
    vm_status: VMStatus,
}

/// VM status enumeration representing the lifecycle states of a virtual machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VMStatus {
    /// VM is being created/loaded
    Loading,
    /// VM is loaded but not yet started
    Loaded,
    /// VM is currently running
    Running,
    /// VM is suspended (paused but can be resumed)
    Suspended,
    /// VM is in the process of shutting down
    Stopping,
    /// VM is stopped
    Stopped,
}

impl VMStatus {
    /// Get status as a string (lowercase)
    pub fn as_str(&self) -> &'static str {
        match self {
            VMStatus::Loading => "loading",
            VMStatus::Loaded => "loaded",
            VMStatus::Running => "running",
            VMStatus::Suspended => "suspended",
            VMStatus::Stopping => "stopping",
            VMStatus::Stopped => "stopped",
        }
    }

    /// Get status with emoji icon
    pub fn as_str_with_icon(&self) -> &'static str {
        match self {
            VMStatus::Loading => "🔄 loading",
            VMStatus::Loaded => "📦 loaded",
            VMStatus::Running => "🚀 running",
            VMStatus::Suspended => "🛑 suspended",
            VMStatus::Stopping => "⏹️ stopping",
            VMStatus::Stopped => "💤 stopped",
        }
    }
}

impl fmt::Display for VMStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

const TEMP_MAX_VCPU_NUM: usize = 64;

/// A Virtual Machine.
pub struct AxVM {
    id: usize,
    inner_const: Once<AxVMInnerConst>,
    inner_mut: Mutex<AxVMInnerMut>,
    pending_fw_cfg: Mutex<Option<PendingFwCfg>>,
}

impl AxVM {
    /// Creates a new VM with the given configuration.
    /// Returns an error if the configuration is invalid.
    /// The VM is not started until `boot` is called.
    pub fn new(config: AxVMConfig) -> AxResult<AxVMRef> {
        let address_space = AddrSpace::new_empty(
            crate::vcpu::max_guest_page_table_levels(),
            GuestPhysAddr::from(VM_ASPACE_BASE),
            VM_ASPACE_SIZE,
        )?;

        let result = Arc::new(Self {
            id: config.id(),
            inner_const: Once::new(),
            inner_mut: Mutex::new(AxVMInnerMut {
                address_space,
                config,
                memory_regions: Vec::new(),
                vm_status: VMStatus::Loading,
            }),
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

    /// Returns the configured VM interrupt mode.
    pub fn interrupt_mode(&self) -> VMInterruptMode {
        self.inner_mut.lock().config.interrupt_mode()
    }

    /// Sets up the VM before booting.
    pub fn init(&self) -> AxResult {
        let mut factories = DeviceFactoryRegistry::new();
        register_builtin_factories(&mut factories)?;
        let interrupt_mode = self.interrupt_mode();
        #[cfg(target_arch = "riscv64")]
        let interrupt_fabric = {
            let inner_mut = self.inner_mut.lock();
            crate::irq::riscv::configure(
                &mut factories,
                interrupt_mode,
                inner_mut.config.emu_devices(),
            )?
        };
        #[cfg(not(target_arch = "riscv64"))]
        let interrupt_fabric = InterruptFabric::new(interrupt_mode);

        self.init_with_factories(&factories, interrupt_fabric)
    }

    /// Sets up the VM with explicit device factories and an interrupt fabric.
    pub fn init_with_factories(
        &self,
        factories: &DeviceFactoryRegistry,
        interrupt_fabric: InterruptFabric,
    ) -> AxResult {
        let mut inner_mut = self.inner_mut.lock();
        interrupt_fabric.validate_mode(inner_mut.config.interrupt_mode())?;

        let dtb_addr = inner_mut.config.image_config().dtb_load_gpa;
        let vcpu_id_pcpu_sets = inner_mut.config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids();
        #[cfg(target_arch = "loongarch64")]
        let loongarch_iocsr_state = {
            let vcpu_state_count = vcpu_id_pcpu_sets
                .iter()
                .map(|(vcpu_id, ..)| *vcpu_id)
                .max()
                .map_or(0, |vcpu_id| vcpu_id + 1);
            loongarch_vcpu::LoongArchIocsrState::new(vcpu_state_count)?
        };

        debug!("dtb_load_gpa: {dtb_addr:?}");
        debug!("id: {}, VCpuIdPCpuSets: {vcpu_id_pcpu_sets:#x?}", self.id());

        let mut vcpu_list = Vec::with_capacity(vcpu_id_pcpu_sets.len());
        for (vcpu_id, phys_cpu_set, _pcpu_id) in vcpu_id_pcpu_sets {
            #[cfg(target_arch = "aarch64")]
            let arch_config = AxVCpuCreateConfig {
                mpidr_el1: _pcpu_id as _,
                dtb_addr: dtb_addr.unwrap_or_default().as_usize(),
            };
            #[cfg(target_arch = "riscv64")]
            let arch_config = AxVCpuCreateConfig {
                hart_id: vcpu_id as _,
                dtb_addr: dtb_addr.unwrap_or_default().as_usize(),
            };
            #[cfg(target_arch = "loongarch64")]
            let arch_config = AxVCpuCreateConfig {
                cpu_id: vcpu_id,
                dtb_addr: dtb_addr.unwrap_or_default().as_usize(),
                boot_args: inner_mut.config.cpu_config.boot_args,
                boot_stack_top: inner_mut.config.cpu_config.boot_stack_top,
                firmware_boot: inner_mut.config.cpu_config.firmware_boot,
                iocsr_state: loongarch_iocsr_state.clone(),
            };

            // FIXME: VCpu is neither `Send` nor `Sync` by design, check whether
            // 1. we should make it `Send` and `Sync`, or
            // 2. we can guarantee that no cross-thread access is performed
            #[allow(clippy::arc_with_non_send_sync)]
            vcpu_list.push(Arc::new(VCpu::new(
                self.id(),
                vcpu_id,
                0, // Currently not used.
                phys_cpu_set,
                #[cfg(target_arch = "aarch64")]
                arch_config,
                #[cfg(target_arch = "loongarch64")]
                arch_config,
                #[cfg(target_arch = "riscv64")]
                arch_config,
                #[cfg(target_arch = "x86_64")]
                (),
            )?));
        }

        let mut pt_dev_region = Vec::new();
        for pt_device in inner_mut.config.pass_through_devices() {
            trace!(
                "PT dev {:?} region: [{:#x}~{:#x}] -> [{:#x}~{:#x}]",
                pt_device.name,
                pt_device.base_gpa,
                pt_device.base_gpa + pt_device.length,
                pt_device.base_hpa,
                pt_device.base_hpa + pt_device.length
            );
            // Align the base address and length to 4K boundaries.
            pt_dev_region.push((
                align_down_4k(pt_device.base_gpa),
                align_up_4k(pt_device.length),
            ));
        }

        for pt_addr in inner_mut.config.pass_through_addresses() {
            debug!(
                "PT addr region: [{:#x}~{:#x}]",
                pt_addr.base_gpa,
                pt_addr.base_gpa + pt_addr.length,
            );
            // Align the base address and length to 4K boundaries.
            pt_dev_region.push((align_down_4k(pt_addr.base_gpa), align_up_4k(pt_addr.length)));
        }

        pt_dev_region.sort_by_key(|(gpa, _)| *gpa);

        // Merge overlapping regions.
        let pt_dev_region =
            pt_dev_region
                .into_iter()
                .fold(Vec::<(usize, usize)>::new(), |mut acc, (gpa, len)| {
                    if let Some(last) = acc.last_mut() {
                        if last.0 + last.1 >= gpa {
                            // Merge with the last region.
                            last.1 = (last.0 + last.1).max(gpa + len) - last.0;
                        } else {
                            acc.push((gpa, len));
                        }
                    } else {
                        acc.push((gpa, len));
                    }
                    acc
                });

        for (gpa, len) in &pt_dev_region {
            inner_mut.address_space.map_linear(
                GuestPhysAddr::from(*gpa),
                HostPhysAddr::from(*gpa),
                *len,
                MappingFlags::DEVICE
                    | MappingFlags::READ
                    | MappingFlags::WRITE
                    | MappingFlags::USER,
            )?;
        }

        #[cfg(all(target_arch = "x86_64", feature = "vmx"))]
        inner_mut.address_space.map_linear(
            GuestPhysAddr::from(X86_APIC_ACCESS_GPA),
            x86_apic_access_page_addr(),
            ax_memory_addr::PAGE_SIZE_4K,
            MappingFlags::DEVICE | MappingFlags::READ | MappingFlags::WRITE,
        )?;

        #[cfg_attr(
            not(any(target_arch = "aarch64", target_arch = "x86_64")),
            expect(unused_mut)
        )]
        let mut devices = {
            let build_context = DeviceBuildContext::new(&interrupt_fabric);
            axdevice::AxVmDevices::build_with_factories(
                AxVmDeviceConfig {
                    emu_configs: inner_mut.config.emu_devices().to_vec(),
                },
                factories,
                &build_context,
            )?
        };

        #[cfg(target_arch = "x86_64")]
        for port in inner_mut.config.pass_through_ports() {
            let passthrough = Arc::new(crate::host::x86_port::HostPortPassthrough::new(
                port.base,
                port.length,
            )?);
            let range = passthrough.address_range();
            debug!(
                "PT port region: [{:#x}~{:#x}]",
                range.start.number(),
                range.end.number(),
            );
            devices
                .register(PortDeviceAdapter::from_arc(passthrough))
                .map_err(|err| ax_err_type!(InvalidInput, format!("register PT port: {err:?}")))?;
        }

        #[cfg(target_arch = "aarch64")]
        {
            let passthrough = inner_mut.config.interrupt_mode() == VMInterruptMode::Passthrough;
            if passthrough {
                let spis = inner_mut.config.pass_through_spis();
                let cpu_id = self.id() - 1; // FIXME: get the real CPU id.
                let mut gicd_found = false;

                for device in devices.devices() {
                    if let Some(gicd) = device.as_any().downcast_ref::<arm_vgic::v3::vgicd::VGicD>()
                    {
                        debug!("VGicD found, assigning SPIs...");

                        for spi in spis {
                            gicd.assign_irq(*spi + 32, cpu_id, (0, 0, 0, cpu_id as _))
                        }

                        gicd_found = true;
                        break;
                    }
                }

                if !gicd_found {
                    warn!("Failed to assign SPIs: No VGicD found in device list");
                }
            } else {
                // non-passthrough mode, we need to set up the virtual timer.
                #[cfg(target_arch = "aarch64")]
                for dev in axdevice::create_vtimer_devices() {
                    devices
                        .register(Arc::from(dev) as Arc<dyn axdevice_base::Device>)
                        .map_err(|e| {
                            ax_err_type!(InvalidInput, format!("register vtimer: {e:?}"))
                        })?;
                }
                #[cfg(not(target_arch = "aarch64"))]
                let _ = (); // silence unused warning on non-aarch64
            }
        }

        self.add_special_emulated_devices(&mut devices)?;

        // Setup VCpus.
        for vcpu in &vcpu_list {
            #[cfg(target_arch = "aarch64")]
            let setup_config = {
                let passthrough = inner_mut.config.interrupt_mode() == VMInterruptMode::Passthrough;
                crate::vcpu::AxVCpuSetupConfig {
                    passthrough_interrupt: passthrough,
                    passthrough_timer: passthrough,
                }
            };
            #[cfg(target_arch = "loongarch64")]
            let setup_config = {
                let passthrough = inner_mut.config.interrupt_mode() == VMInterruptMode::Passthrough;
                crate::vcpu::AxVCpuSetupConfig {
                    passthrough_interrupt: passthrough,
                    passthrough_timer: passthrough,
                    boot_args: inner_mut.config.cpu_config.boot_args,
                    boot_stack_top: inner_mut.config.cpu_config.boot_stack_top,
                    firmware_boot: inner_mut.config.cpu_config.firmware_boot,
                }
            };
            #[cfg(not(any(
                target_arch = "aarch64",
                target_arch = "loongarch64",
                target_arch = "x86_64"
            )))]
            #[allow(clippy::let_unit_value)]
            let setup_config = <AxArchVCpuImpl as axvcpu::AxArchVCpu>::SetupConfig::default();
            #[cfg(target_arch = "x86_64")]
            let setup_config = {
                let mut config = crate::vcpu::AxVCpuSetupConfig {
                    emulate_com1: inner_mut
                        .config
                        .emu_devices()
                        .iter()
                        .any(|dev| dev.emu_type == EmulatedDeviceType::Console),
                    ..Default::default()
                };
                for port in inner_mut.config.pass_through_ports() {
                    config.add_passthrough_port_range(port.base, port.length)?;
                }
                config
            };

            let entry = if vcpu.id() == 0 {
                inner_mut.config.bsp_entry()
            } else {
                inner_mut.config.ap_entry()
            };

            debug!("Setting up vCPU[{}] entry at {:#x}", vcpu.id(), entry);

            vcpu.setup(
                entry,
                inner_mut.address_space.page_table_root(),
                setup_config,
            )?;
        }

        self.inner_const.call_once(|| AxVMInnerConst {
            phys_cpu_ls: inner_mut.config.phys_cpu_ls.clone(),
            vcpu_list: vcpu_list.into_boxed_slice(),
            devices,
            interrupt_fabric,
        });

        info!("VM setup: id={}", self.id());
        Ok(())
    }

    /// Sets the VM status.
    pub fn set_vm_status(&self, status: VMStatus) {
        let mut inner_mut = self.inner_mut.lock();
        inner_mut.vm_status = status;
    }

    /// Returns the current VM status.
    pub fn vm_status(&self) -> VMStatus {
        let inner_mut = self.inner_mut.lock();
        inner_mut.vm_status
    }

    /// Retrieves the vCPU corresponding to the given vcpu_id for the VM.
    /// Returns None if the vCPU does not exist.
    #[inline]
    pub fn vcpu(&self, vcpu_id: usize) -> Option<AxVCpuRef> {
        self.vcpu_list().get(vcpu_id).cloned()
    }

    /// Returns the number of vCPUs corresponding to the VM.
    #[inline]
    pub fn vcpu_num(&self) -> usize {
        self.inner_const().vcpu_list.len()
    }

    fn inner_const(&self) -> &AxVMInnerConst {
        self.inner_const
            .get()
            .expect("VM inner_const not initialized")
    }

    /// Returns a reference to the list of vCPUs corresponding to the VM.
    #[inline]
    pub fn vcpu_list(&self) -> &[AxVCpuRef] {
        &self.inner_const().vcpu_list
    }

    /// Returns the base address of the two-stage address translation page table for the VM.
    pub fn ept_root(&self) -> HostPhysAddr {
        self.inner_mut.lock().address_space.page_table_root()
    }

    /// Returns to the VM's configuration.
    pub fn with_config<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut AxVMConfig) -> R,
    {
        let mut g = self.inner_mut.lock();
        f(&mut g.config)
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
        let g = self.inner_mut.lock();
        let image_load_hva = g
            .address_space
            .translated_byte_buffer(image_load_gpa, image_size)
            .expect("Failed to translate kernel image load address");
        Ok(image_load_hva)
    }

    /// Boots the VM by transitioning to Running state.
    pub fn boot(&self) -> AxResult {
        if self.running() {
            ax_err!(BadState, format!("VM[{}] is already running", self.id()))
        } else {
            info!("Booting VM[{}]", self.id());
            self.set_vm_status(VMStatus::Running);
            Ok(())
        }
    }

    /// Returns if the VM is running.
    pub fn running(&self) -> bool {
        self.vm_status() == VMStatus::Running
    }

    /// Returns if the VM is shutting down (in Stopping state).
    pub fn stopping(&self) -> bool {
        self.vm_status() == VMStatus::Stopping
    }

    /// Returns if the VM is suspended.
    pub fn suspending(&self) -> bool {
        self.vm_status() == VMStatus::Suspended
    }

    /// Returns if the VM is stopped.
    pub fn stopped(&self) -> bool {
        self.vm_status() == VMStatus::Stopped
    }

    /// Shuts down the VM by transitioning to Stopping state.
    ///
    /// This method sets the VM status to Stopping, which signals all vCPUs to exit.
    /// Currently, the "re-init" process of the VM is not implemented. Therefore, a VM can only be
    /// booted once. And after the VM is shut down, it cannot be booted again.
    pub fn shutdown(&self) -> AxResult {
        if self.stopping() {
            ax_err!(BadState, format!("VM[{}] is already stopping", self.id()))
        } else if self.stopped() {
            ax_err!(BadState, format!("VM[{}] is already stopped", self.id()))
        } else {
            info!("Shutting down VM[{}]", self.id());
            self.set_vm_status(VMStatus::Stopping);
            Ok(())
        }
    }

    // TODO: implement suspend/resume.
    // TODO: implement re-init.

    /// Returns this VM's emulated devices.
    pub fn get_devices(&self) -> &AxVmDevices {
        &self.inner_const().devices
    }

    /// Returns this VM's interrupt fabric.
    pub fn interrupt_fabric(&self) -> &InterruptFabric {
        &self.inner_const().interrupt_fabric
    }

    /// Assert a routed LoongArch platform IRQ in the guest interrupt model.
    #[cfg(target_arch = "loongarch64")]
    pub(crate) fn loongarch_external_irq_vector(
        &self,
        fallback_vector: usize,
        _physical_irq: usize,
    ) -> Option<usize> {
        match self
            .get_devices()
            .loongarch_pch_pic_assert_irq(fallback_vector)
        {
            Some(Some(vector)) => Some(vector),
            Some(None) => None,
            None => Some(fallback_vector),
        }
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

    /// Run a vCPU according to the given vcpu_id.
    ///
    /// ## Arguments
    /// * `vcpu_id` - the id of the vCPU to run.
    ///
    /// ## Returns
    /// * `AxVCpuExitReason` - the exit reason of the vCPU, wrapped in an `AxResult`.
    pub fn run_vcpu(&self, vcpu_id: usize) -> AxResult<AxVCpuExitReason> {
        let vm_id = self.id();
        let vcpu = self
            .vcpu(vcpu_id)
            .ok_or_else(|| ax_err_type!(InvalidInput, "Invalid vcpu_id"))?;

        match vcpu.state() {
            VCpuState::Free => vcpu.bind()?,
            VCpuState::Ready => {}
            state => {
                return ax_err!(
                    BadState,
                    format!("VCpu state is not Free or Ready, but {state:?}")
                );
            }
        }

        let run_result = vcpu.with_current_cpu_set(|| -> AxResult<AxVCpuExitReason> {
            loop {
                crate::runtime::vcpus::inject_pending_interrupts(self.id(), vcpu_id, &vcpu);

                let exit_reason = vcpu.run()?;
                trace!("{exit_reason:#x?}");
                match exit_reason {
                    AxVCpuExitReason::MmioRead {
                        addr,
                        width,
                        reg,
                        reg_width,
                        signed_ext,
                    } => {
                        let raw = self.get_devices().handle_mmio_read(addr, width)?;
                        let masked = raw & width_mask(width);
                        let val = if signed_ext {
                            sign_extend_value(masked, width)
                        } else {
                            masked & width_mask(reg_width)
                        };
                        vcpu.set_gpr(reg, val);
                    }
                    AxVCpuExitReason::MmioWrite { addr, width, data } => {
                        self.handle_mmio_write(addr, width, data as usize)?;
                    }
                    AxVCpuExitReason::IoRead { port, width } => {
                        let val = self.get_devices().handle_port_read(port, width)?;
                        #[cfg(not(target_arch = "riscv64"))]
                        vcpu.set_gpr(0, val); // The target is always eax/ax/al, todo: handle access_width correctly

                        #[cfg(target_arch = "riscv64")]
                        vcpu.set_gpr(riscv_vcpu::GprIndex::A0 as usize, val);
                    }
                    AxVCpuExitReason::IoWrite { port, width, data } => {
                        self.get_devices()
                            .handle_port_write(port, width, data as usize)?;
                    }
                    AxVCpuExitReason::SysRegRead { addr, reg } => {
                        let val = self.get_devices().handle_sys_reg_read(
                            addr,
                            // Generally speaking, the width of system register is fixed and needless to be specified.
                            // AccessWidth::Qword here is just a placeholder, may be changed in the future.
                            AccessWidth::Qword,
                        )?;
                        vcpu.set_gpr(reg, val);
                    }
                    AxVCpuExitReason::SysRegWrite { addr, value } => {
                        self.get_devices().handle_sys_reg_write(
                            addr,
                            AccessWidth::Qword,
                            value as usize,
                        )?;
                    }
                    AxVCpuExitReason::NestedPageFault { addr, access_flags } => {
                        if self.get_devices().find_mmio_dev(addr).is_some() {
                            if let Some(mmio_exit) =
                                vcpu.get_arch_vcpu().decode_mmio_fault(addr, access_flags)
                            {
                                match mmio_exit {
                                    AxVCpuExitReason::MmioRead {
                                        addr,
                                        width,
                                        reg,
                                        reg_width,
                                        signed_ext,
                                    } => {
                                        let raw =
                                            self.get_devices().handle_mmio_read(addr, width)?;
                                        let masked = raw & width_mask(width);
                                        let val = if signed_ext {
                                            sign_extend_value(masked, width)
                                        } else {
                                            masked & width_mask(reg_width)
                                        };
                                        vcpu.set_gpr(reg, val);
                                    }
                                    AxVCpuExitReason::MmioWrite { addr, width, data } => {
                                        self.handle_mmio_write(addr, width, data as usize)?;
                                    }
                                    exit_reason => break Ok(exit_reason),
                                }
                            } else {
                                break Ok(AxVCpuExitReason::NestedPageFault { addr, access_flags });
                            }
                        } else if !self.handle_nested_page_fault(addr, access_flags) {
                            break Ok(AxVCpuExitReason::NestedPageFault { addr, access_flags });
                        }
                    }
                    exit_reason => break Ok(exit_reason),
                }
            }
        });

        let unbind_result = vcpu.unbind();
        match run_result {
            Ok(exit_reason) => {
                unbind_result?;
                Ok(exit_reason)
            }
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

    fn handle_mmio_write(&self, addr: GuestPhysAddr, width: AccessWidth, data: usize) -> AxResult {
        if let Some(fw_cfg) = self.get_devices().fw_cfg_for_dma_addr(addr) {
            if let Some(desc_addr) = fw_cfg.write_dma_address(addr, width, data)? {
                fw_cfg.process_dma(
                    desc_addr,
                    |gpa, buffer| self.read_from_guest(gpa, buffer),
                    |gpa, buffer| self.write_to_guest(gpa, buffer),
                )?;
            }
            return Ok(());
        }

        self.get_devices().handle_mmio_write(addr, width, data)?;
        #[cfg(target_arch = "loongarch64")]
        self.drain_loongarch_pch_pic_events();
        Ok(())
    }

    #[cfg(target_arch = "loongarch64")]
    fn drain_loongarch_pch_pic_events(&self) {
        self.get_devices().drain_loongarch_pch_pic_events(|event| {
            if !event.asserted {
                trace!(
                    "LoongArch VM[{}] PCH-PIC deassert event for EIOINTC vector {}",
                    self.id(),
                    event.vector
                );
                return;
            }
            if let Err(err) = crate::manager::inject_vm_vcpu_interrupt(self.id(), 0, event.vector) {
                warn!(
                    "failed to inject LoongArch VM[{}] PCH-PIC output vector {}: {err:?}",
                    self.id(),
                    event.vector
                );
            }
        });
    }

    fn handle_nested_page_fault(&self, addr: GuestPhysAddr, access_flags: MappingFlags) -> bool {
        let mut guard = self.inner_mut.lock();
        let handled = guard.address_space.handle_page_fault(addr, access_flags);
        Self::debug_nested_page_fault(self.id(), &guard, addr, access_flags, handled);
        handled
    }

    fn debug_nested_page_fault(
        vm_id: usize,
        inner: &AxVMInnerMut,
        addr: GuestPhysAddr,
        access_flags: MappingFlags,
        handled: bool,
    ) {
        let root = inner.address_space.page_table_root();
        match inner.address_space.page_table().query(addr) {
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

        let translate = inner.address_space.translate(addr);
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

        for (idx, region) in inner.memory_regions.iter().enumerate() {
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
        self.inner_const()
            .phys_cpu_ls
            .get_vcpu_affinities_pcpu_ids()
    }

    // /// Returns a reference to the VM's configuration.
    // pub fn config(&self) -> &AxVMConfig {
    //     &self.inner_const.config
    // }

    /// Maps a region of host physical memory to guest physical memory.
    pub fn map_region(
        &self,
        gpa: GuestPhysAddr,
        hpa: HostPhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> AxResult {
        self.inner_mut
            .lock()
            .address_space
            .map_linear(gpa, hpa, size, flags)?;
        Ok(())
    }

    /// Unmaps a region of guest physical memory.
    pub fn unmap_region(&self, gpa: GuestPhysAddr, size: usize) -> AxResult {
        self.inner_mut.lock().address_space.unmap(gpa, size)?;
        Ok(())
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

        let g = self.inner_mut.lock();
        match g.address_space.translated_byte_buffer(gpa_ptr, size) {
            Some(buffers) => {
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
            }
            None => ax_err!(
                InvalidInput,
                "Failed to translate guest physical address or insufficient buffer size"
            ),
        }
    }

    /// Reads raw bytes from guest physical memory.
    pub fn read_from_guest(&self, gpa_ptr: GuestPhysAddr, buffer: &mut [u8]) -> AxResult {
        let g = self.inner_mut.lock();
        let Some(chunks) = g
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
    }

    /// Writes an object of type `T` to the guest physical address.
    pub fn write_to_guest_of<T>(&self, gpa_ptr: GuestPhysAddr, data: &T) -> AxResult {
        match self
            .inner_mut
            .lock()
            .address_space
            .translated_byte_buffer(gpa_ptr, core::mem::size_of::<T>())
        {
            Some(mut buffer) => {
                let bytes = unsafe {
                    core::slice::from_raw_parts(
                        data as *const T as *const u8,
                        core::mem::size_of::<T>(),
                    )
                };
                let mut copied_bytes = 0;
                for chunk in buffer.iter_mut() {
                    let end = copied_bytes + chunk.len();
                    chunk.copy_from_slice(&bytes[copied_bytes..end]);
                    copied_bytes += chunk.len();
                }
                Ok(())
            }
            None => ax_err!(InvalidInput, "Failed to translate guest physical address"),
        }
    }

    /// Writes raw bytes into guest physical memory.
    pub fn write_to_guest(&self, gpa_ptr: GuestPhysAddr, data: &[u8]) -> AxResult {
        let g = self.inner_mut.lock();
        let Some(mut chunks) = g.address_space.translated_byte_buffer(gpa_ptr, data.len()) else {
            return ax_err!(InvalidInput, "Failed to translate guest physical address");
        };

        let mut copied = 0;
        for chunk in chunks.iter_mut() {
            let len = (data.len() - copied).min(chunk.len());
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

    /// Allocates an IVC channel for inter-VM communication region.
    ///
    /// ## Arguments
    /// * `expected_size` - The expected size of the IVC channel in bytes.
    /// ## Returns
    /// * `AxResult<(GuestPhysAddr, usize)>` - A tuple containing the guest physical address of the allocated IVC channel and its actual size.
    pub fn alloc_ivc_channel(&self, expected_size: usize) -> AxResult<(GuestPhysAddr, usize)> {
        // Ensure the expected size is aligned to 4K.
        let size = align_up_4k(expected_size);
        let gpa = self.inner_const().devices.alloc_ivc_channel(size)?;
        Ok((gpa, size))
    }

    /// Releases an IVC channel for inter-VM communication region.
    /// ## Arguments
    /// * `gpa` - The guest physical address of the IVC channel to release.
    /// * `size` - The size of the IVC channel in bytes.
    /// ## Returns
    /// * `AxResult<()>` - An empty result indicating success or failure.
    pub fn release_ivc_channel(&self, gpa: GuestPhysAddr, size: usize) -> AxResult {
        self.inner_const().devices.release_ivc_channel(gpa, size)?;
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

        let mut g = self.inner_mut.lock();
        g.address_space.map_linear(
            gpa,
            hpa,
            layout.size(),
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE | MappingFlags::USER,
        )?;
        g.memory_regions.push(VMMemoryRegion {
            gpa,
            hva,
            layout,
            needs_dealloc: true, // This region was allocated and needs to be freed
        });

        Ok(s)
    }

    /// Returns a list of all memory regions in the VM.
    pub fn memory_regions(&self) -> Vec<VMMemoryRegion> {
        self.inner_mut.lock().memory_regions.clone()
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
        let mut g = self.inner_mut.lock();
        g.address_space.map_linear(
            gpa,
            gpa.as_usize().into(),
            layout.size(),
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE | MappingFlags::USER,
        )?;
        let hva = gpa.as_usize().into();
        g.memory_regions.push(VMMemoryRegion {
            gpa,
            hva,
            layout,
            needs_dealloc: false, // This is a reserved region, not allocated
        });
        Ok(())
    }

    /// Cleanup resources for the VM before drop.
    /// This is called internally by the Drop implementation.
    fn cleanup_resources(&self) {
        info!("Cleaning up VM[{}] resources...", self.id());

        // 1. Ensure the VM is in Stopping or Stopped state
        let current_status = self.vm_status();
        if !matches!(current_status, VMStatus::Stopping | VMStatus::Stopped) {
            warn!(
                "VM[{}] is being dropped without explicit shutdown (status: {:?}), marking as \
                 stopping",
                self.id(),
                current_status
            );
            self.set_vm_status(VMStatus::Stopping);
        }

        let mut inner_mut = self.inner_mut.lock();

        // First, collect all memory regions to clean up
        // We need to clone the regions to avoid borrowing issues
        let regions_to_cleanup: Vec<VMMemoryRegion> = inner_mut.memory_regions.clone();

        // Unmap all memory regions from the address space
        // This must be done BEFORE deallocating memory to avoid use-after-free
        for region in &regions_to_cleanup {
            debug!(
                "VM[{}] unmapping memory region: GPA={:#x}, size={:#x}",
                self.id(),
                region.gpa.as_usize(),
                region.size()
            );
            // Unmap the region from guest physical address space
            if let Err(e) = inner_mut.address_space.unmap(region.gpa, region.size()) {
                warn!(
                    "VM[{}] failed to unmap region at GPA={:#x}: {:?}",
                    self.id(),
                    region.gpa.as_usize(),
                    e
                );
            }
        }

        // Now it's safe to deallocate the memory
        for region in &regions_to_cleanup {
            // Only deallocate memory regions that were allocated by the allocator
            if region.needs_dealloc {
                debug!(
                    "VM[{}] deallocating memory region: HVA={:#x}, size={:#x}",
                    self.id(),
                    region.hva.as_usize(),
                    region.size()
                );
                unsafe {
                    alloc::alloc::dealloc(region.hva.as_mut_ptr(), region.layout);
                }
            } else {
                debug!(
                    "VM[{}] skipping dealloc for reserved memory region: GPA={:#x}, HVA={:#x}, \
                     size={:#x}",
                    self.id(),
                    region.gpa.as_usize(),
                    region.hva.as_usize(),
                    region.size()
                );
            }
        }
        inner_mut.memory_regions.clear();

        // Clear remaining address space mappings
        // This includes:
        // - Passthrough device MMIO mappings
        // - Emulated device MMIO mappings
        // - Reserved memory mappings
        // - All other page table entries
        debug!(
            "VM[{}] clearing remaining address space mappings",
            self.id()
        );
        inner_mut.address_space.clear();

        // Release the lock before accessing inner_const
        drop(inner_mut);

        // Device cleanup
        // Although devices will be automatically dropped when inner_const is dropped,
        // we should perform explicit cleanup if devices hold resources like:
        // - Hardware interrupt registrations
        // - DMA mappings
        // - Background threads or timers
        if let Some(inner_const) = self.inner_const.get() {
            debug!(
                "VM[{}] devices cleanup: {} device(s)",
                self.id(),
                inner_const.devices.devices().count()
            );

            // TODO: Add device-specific cleanup if needed
            // For example:
            // - Stop device background tasks
            // - Unregister interrupts
            // - Release device-specific resources

            // Note: Device Arc references will be dropped automatically when
            // inner_const is dropped at the end of AxVM's drop
        }

        info!("VM[{}] resources cleanup completed", self.id());
    }
}

impl Drop for AxVM {
    fn drop(&mut self) {
        info!("Dropping VM[{}]", self.id());

        // Clean up all allocated resources
        self.cleanup_resources();

        info!("VM[{}] dropped", self.id());
    }
}

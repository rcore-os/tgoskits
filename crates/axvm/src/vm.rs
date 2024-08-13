use alloc::boxed::Box;
use alloc::format;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;

use axerrno::{ax_err, ax_err_type, AxResult};
use memory_addr::VirtAddr;
use spin::Mutex;

use axvcpu::{AxArchVCpu, AxVCpu};

use axaddrspace::{AddrSpace, GuestPhysAddr, HostPhysAddr, MappingFlags};

use crate::arch::{AxArchDeviceList, AxArchVCpuImpl};
use crate::config::AxVMConfig;
use crate::{has_hardware_support, AxVMHal};

const VM_ASPACE_BASE: usize = 0x0;
const VM_ASPACE_SIZE: usize = 0x7fff_ffff_f000;

#[allow(type_alias_bounds)] // we know the bound is not enforced here, we keep it for clarity
type VCpu<H: AxVMHal> = AxVCpu<AxArchVCpuImpl<H>>;

struct AxVMInnerConst<H: AxVMHal> {
    id: usize,
    config: AxVMConfig,
    vcpu_list: Box<[VCpu<H>]>,
    // to be added: device_list: ...
    device_list: UnsafeCell<AxArchDeviceList<H>>,
}

struct AxVMInnerMut<H: AxVMHal> {
    // Todo: use more efficient lock.
    address_space: Mutex<AddrSpace<H::PagingHandler>>,
    _marker: core::marker::PhantomData<H>,
}

/// A Virtual Machine.
pub struct AxVM<H: AxVMHal> {
    inner_const: AxVMInnerConst<H>,
    inner_mut: AxVMInnerMut<H>,
}

impl<H: AxVMHal> AxVM<H> {
    pub fn new(config: AxVMConfig) -> AxResult<Arc<Self>> {
        let result = Arc::new({
            let vcpu_pcpu_id_pairs = config.get_vcpu_pcpu_id_pairs();

            // Create VCpus.
            let mut vcpu_list = Vec::with_capacity(vcpu_pcpu_id_pairs.len());

            for (vcpu_id, pcpu_id) in vcpu_pcpu_id_pairs {
                // Todo: distinguish between `favor_phys_cpu` and `affinity`.
                vcpu_list.push(VCpu::new(
                    vcpu_id,
                    pcpu_id,
                    pcpu_id,
                    <AxArchVCpuImpl<H> as AxArchVCpu>::CreateConfig::default(),
                )?);
            }

            // Set up Memory regions.
            let mut address_space =
                AddrSpace::new_empty(VirtAddr::from(VM_ASPACE_BASE), VM_ASPACE_SIZE)?;
            for mem_region in config.memory_regions() {
                let mapping_flags = MappingFlags::from_bits(mem_region.flags).ok_or_else(|| {
                    ax_err_type!(
                        InvalidInput,
                        format!("Illegal flags {:?}", mem_region.flags)
                    )
                })?;

                // Handle passthrough device's memory region.
                // Todo: Perhaps we can merge the management of passthrough device memory
                //       into the device configuration file.
                if mapping_flags.contains(MappingFlags::DEVICE) {
                    address_space.map_linear(
                        GuestPhysAddr::from(mem_region.gpa),
                        HostPhysAddr::from(mem_region.gpa),
                        mem_region.size,
                        mapping_flags,
                    )?;
                } else {
                    // Handle ram region.
                    // Note: currently we use `map_alloc`,
                    // which allocates real physical memory in units of physical page frames,
                    // which may not be contiguous!!!
                    address_space.map_alloc(
                        GuestPhysAddr::from(mem_region.gpa),
                        mem_region.size,
                        mapping_flags,
                        true,
                    )?;
                }
            }

            // Setup Devices.
            // Todo:
            let device_list = AxArchDeviceList::<H>::new();

            Self {
                inner_const: AxVMInnerConst {
                    id: config.id(),
                    config,
                    vcpu_list: vcpu_list.into_boxed_slice(),
                    device_list: UnsafeCell::new(device_list),
                },
                inner_mut: AxVMInnerMut {
                    address_space: Mutex::new(address_space),
                    _marker: core::marker::PhantomData,
                },
            }
        });

        info!("VM created: id={}", result.id());

        // Setup VCpus.
        for vcpu in result.vcpu_list() {
            let entry = if vcpu.id() == 0 {
                result.inner_const.config.bsp_entry()
            } else {
                result.inner_const.config.ap_entry()
            };
            vcpu.setup(
                entry.as_usize(),
                result.ept_root(),
                <AxArchVCpuImpl<H> as AxArchVCpu>::SetupConfig::default(),
            )?;
        }
        info!("VM setup: id={}", result.id());

        Ok(result)
    }

    /// Returns the VM id.
    #[inline]
    pub fn id(&self) -> usize {
        self.inner_const.id
    }

    /// Retrieves the vCPU corresponding to the given vcpu_id for the VM.
    /// Returns None if the vCPU does not exist.
    #[inline]
    pub fn vcpu(&self, vcpu_id: usize) -> Option<&VCpu<H>> {
        self.vcpu_list().get(vcpu_id)
    }

    /// Returns a reference to the list of vCPUs corresponding to the VM.
    #[inline]
    pub fn vcpu_list(&self) -> &[VCpu<H>] {
        &self.inner_const.vcpu_list
    }

    /// Returns the base address of the two-stage address translation page table for the VM.
    pub fn ept_root(&self) -> HostPhysAddr {
        self.inner_mut.address_space.lock().page_table_root()
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
        let addr_space = self.inner_mut.address_space.lock();
        let image_load_hva = addr_space
            .translated_byte_buffer(image_load_gpa, image_size)
            .expect("Failed to translate kernel image load address");
        Ok(image_load_hva)
    }

    pub fn boot(&self) -> AxResult {
        if !has_hardware_support() {
            ax_err!(Unsupported, "Hardware does not support virtualization")
        } else {
            self.run_vcpu(0)
        }
    }

    pub fn get_device_list(&self) -> &mut AxArchDeviceList<H> {
        unsafe { &mut *self.inner_const.device_list.get() }
    }

    pub fn run_vcpu(&self, vcpu_id: usize) -> AxResult {
        let vcpu = self
            .vcpu(vcpu_id)
            .ok_or_else(|| ax_err_type!(InvalidInput, "Invalid vcpu_id"))?;
        vcpu.bind()?;
        loop {
            // todo: device access
            let exit_reason = vcpu.run()?;

            match exit_reason {
                axvcpu::AxArchVCpuExitReason::MmioRead { addr, width: _ } => {
                    debug!("EPT from addr {:#x}", addr);

                    let paddr = self
                        .inner_mut
                        .address_space
                        .lock()
                        .translate(VirtAddr::from(addr));

                    debug!("EPT mapped to {:#x?}", paddr);

                    break;
                }
                _ => {
                    let device_list = self.get_device_list();
                    device_list.vmexit_handler(vcpu.get_arch_vcpu(), exit_reason)?;
                }
            }
        }
        vcpu.unbind()?;
        panic!("VCpu [{}] halt", vcpu.id())
    }
}

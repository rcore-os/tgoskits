#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]
#![feature(naked_functions)]
#![allow(warnings)]
#[macro_use]
#[cfg(feature = "axstd")]
extern crate axstd as std;

extern crate alloc;

#[macro_use]
extern crate log;

#[cfg(target_arch = "x86_64")]
mod device_emu;
mod gconfig;
mod gpm;
mod hal;
#[cfg(target_arch = "x86_64")]
mod vmexit;

#[cfg(target_arch = "aarch64")]
mod dtb_aarch64;

use alloc::vec::Vec;

use axerrno::{AxError, AxResult};
use axhal::mem::virt_to_phys;
use axhal::paging::{PageSize, PagingIfImpl};
use axvm::arch::AxArchVCpuConfig;
use axvm::config::{AxVCpuConfig, AxVMConfig};
use axvm::AxVMPerCpu;
use axvm::{AxVM, GuestPhysAddr, HostPhysAddr, HostVirtAddr};
use page_table_entry::MappingFlags;

use self::gconfig::*;
use self::gpm::{GuestMemoryRegion, GuestPhysMemorySet};
use self::hal::AxVMHalImpl;
use alloc::vec;

#[cfg(target_arch = "aarch64")]
use dtb_aarch64::MachineMeta;

#[repr(align(4096))]
struct AlignedMemory<const LEN: usize>([u8; LEN]);

static mut GUEST_PHYS_MEMORY: AlignedMemory<GUEST_PHYS_MEMORY_SIZE> =
    AlignedMemory([0; GUEST_PHYS_MEMORY_SIZE]);

fn gpa_as_mut_ptr(guest_paddr: GuestPhysAddr) -> *mut u8 {
    let offset = unsafe { core::ptr::addr_of!(GUEST_PHYS_MEMORY) as *const _ as usize };
    debug!("offset: {:#x}", offset);
    let host_vaddr = guest_paddr + offset;
    host_vaddr as *mut u8
}

fn load_guest_image_from_file_system(file_name: &str, load_gpa: GuestPhysAddr) -> AxResult {
    use std::io::{BufReader, Read};
    let file = std::fs::File::open(file_name).map_err(|err| {
        warn!(
            "Failed to open {}, err {:?}, please check your disk.img",
            file_name, err
        );
        AxError::NotFound
    })?;
    debug!("Loading {} to GPA {:#x} gpa_ptr_addr:{:#x}", file_name, load_gpa, gpa_as_mut_ptr(load_gpa) as usize);
    let buffer = unsafe {
        core::slice::from_raw_parts_mut(
            gpa_as_mut_ptr(load_gpa),
            file.metadata()
                .map_err(|err| {
                    warn!(
                        "Failed to get metadate of file {}, err {:?}",
                        file_name, err
                    );
                    AxError::Io
                })?
                .size() as usize,
        )
    };
    let mut file = BufReader::new(file);
    file.read_exact(buffer).map_err(|err| {
        warn!("Failed to read from file {}, err {:?}", file_name, err);
        AxError::Io
    })?;
    Ok(())
}

fn setup_gpm() -> AxResult<GuestPhysMemorySet> {
    // copy BIOS and guest images from file system
    #[cfg(target_arch = "x86_64")]
    load_guest_image_from_file_system("rvm-bios.bin", BIOS_ENTRY)?;

    load_guest_image_from_file_system("nimbos.bin", GUEST_ENTRY)?;

    #[cfg(target_arch = "aarch64")]
    load_guest_image_from_file_system("nimbos-aarch64.dtb", DTB_ENTRY)?;

    // create nested page table and add mapping
    let mut gpm = GuestPhysMemorySet::new()?;
    let mut guest_memory_regions: Vec<GuestMemoryRegion> = vec![];

    #[cfg(target_arch = "aarch64")] 
    {
        let meta = MachineMeta::parse(DTB_ENTRY);
        guest_memory_regions.push(GuestMemoryRegion {
            // RAM
            gpa: GUEST_PHYS_MEMORY_BASE,
            hpa: virt_to_phys(HostVirtAddr::from(
                gpa_as_mut_ptr(GUEST_PHYS_MEMORY_BASE) as usize
            )),
            size: GUEST_PHYS_MEMORY_SIZE,
            flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE,
        });
        // hardcode for virtio
        guest_memory_regions.push(GuestMemoryRegion {
            // virt io
            gpa: 0x0a00_0000,
            hpa: HostPhysAddr::from(0x0a00_0000),
            size: 0x4000,
            flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        });
        if let Some(pl011) = meta.pl011 {
            guest_memory_regions.push(GuestMemoryRegion {
                // uart
                gpa: pl011.base_address as GuestPhysAddr,
                hpa: HostPhysAddr::from(pl011.base_address),
                size: pl011.size,
                flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
            });
        }
        if let Some(pl031) = meta.pl031 {
            guest_memory_regions.push(GuestMemoryRegion {
                // timer
                gpa: pl031.base_address as GuestPhysAddr,
                hpa: HostPhysAddr::from(pl031.base_address),
                size: pl031.size,
                flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
            });
        }
        if let Some(pl061) = meta.pl061 {
            guest_memory_regions.push(GuestMemoryRegion {
                // rtc
                gpa: pl061.base_address as GuestPhysAddr,
                hpa: HostPhysAddr::from(pl061.base_address),
                size: pl061.size,
                flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
            });
        }
        for intc in meta.intc.iter() {
            guest_memory_regions.push(GuestMemoryRegion {
                // gic
                gpa: intc.base_address as GuestPhysAddr,
                hpa: HostPhysAddr::from(intc.base_address),
                size: intc.size,
                flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
            });
        }
        if let Some(pcie) = meta.pcie {
            guest_memory_regions.push(GuestMemoryRegion {
                // pcie
                gpa: pcie.base_address as GuestPhysAddr,
                hpa: HostPhysAddr::from(pcie.base_address),
                size: pcie.size,
                flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
            });
        }
        guest_memory_regions.push(GuestMemoryRegion {
            // physical memory offset
            gpa: 0xffff_0000_4008_0000,
            hpa: virt_to_phys(HostVirtAddr::from(
                gpa_as_mut_ptr(GUEST_ENTRY) as usize
            )),
            size: meta.physical_memory_size,
            flags: MappingFlags::READ
                | MappingFlags::WRITE
                | MappingFlags::EXECUTE
                | MappingFlags::USER,
        });
        // for flash in meta.flash.iter() {
        //     guest_memory_regions.push(GuestMemoryRegion {
        //         // flash
        //         gpa: flash.base_address as GuestPhysAddr,
        //         hpa: HostPhysAddr::from(flash.base_address),
        //         size: flash.size,
        //         flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        //     });
        // }
    }
    

    #[cfg(target_arch = "x86_64")]
    let guest_memory_regions = [
        GuestMemoryRegion {
            // RAM
            gpa: GUEST_PHYS_MEMORY_BASE,
            hpa: virt_to_phys(HostVirtAddr::from(
                gpa_as_mut_ptr(GUEST_PHYS_MEMORY_BASE) as usize
            )),
            size: GUEST_PHYS_MEMORY_SIZE,
            flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE,
        },
        GuestMemoryRegion {
            // IO APIC
            gpa: 0xfec0_0000,
            hpa: HostPhysAddr::from(0xfec0_0000),
            size: 0x1000,
            flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::DEVICE,
        },
        GuestMemoryRegion {
            // HPET
            gpa: 0xfed0_0000,
            hpa: HostPhysAddr::from(0xfed0_0000),
            size: 0x1000,
            flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::DEVICE,
        },
        GuestMemoryRegion {
            // Local APIC
            gpa: 0xfee0_0000,
            hpa: HostPhysAddr::from(0xfee0_0000),
            size: 0x1000,
            flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::DEVICE,
        },
    ];

    // #[cfg(target_arch = "aarch64")] 
    // let mut guest_memory_regions = vec![
    //     GuestMemoryRegion {
    //         // RAM
    //         gpa: GUEST_PHYS_MEMORY_BASE,
    //         hpa: virt_to_phys(HostVirtAddr::from(
    //             gpa_as_mut_ptr(GUEST_PHYS_MEMORY_BASE) as usize
    //         )),
    //         size: GUEST_PHYS_MEMORY_SIZE,
    //         flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE,
    //     },
    //     GuestMemoryRegion {
    //         // virt io
    //         gpa: 0x0a00_0000,
    //         hpa: HostPhysAddr::from(0x0a00_0000),
    //         size: 0x4000,
    //         flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
    //     },
    //     GuestMemoryRegion {
    //         // map gicc to gicv. the address is qemu setting, it is different from real hardware
    //         gpa: 0x0801_0000,
    //         hpa: HostPhysAddr::from(0x0801_0000),
    //         size: 0x2000,
    //         flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
    //     },
    //     GuestMemoryRegion {
    //         //
    //         gpa: 0x0802_0000,
    //         hpa: HostPhysAddr::from(0x0802_0000),
    //         size: 0x1_0000,
    //         flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
    //     },
    //     GuestMemoryRegion {
    //         // physical memory offset
    //         gpa: meta.physical_memory_offset,
    //         hpa: HostPhysAddr::from(meta.physical_memory_offset),
    //         size: meta.physical_memory_size,
    //         flags: MappingFlags::READ
    //             | MappingFlags::WRITE
    //             | MappingFlags::EXECUTE
    //             | MappingFlags::USER,
    //     },
    //     GuestMemoryRegion {
    //         // nimbos guest kernel
    //         gpa: 0xffff_0000_4008_0000,
    //         hpa: HostPhysAddr::from(gpa_as_mut_ptr(GUEST_ENTRY) as usize),
    //         size: meta.physical_memory_size,
    //         flags: MappingFlags::READ
    //             | MappingFlags::WRITE
    //             | MappingFlags::EXECUTE
    //             | MappingFlags::USER,
    //     },
    // ];
    // some devices not map(flash,pcie... )

    for r in guest_memory_regions.into_iter() {
        trace!("{:#x?}", r);
        gpm.map_region(r.into())?;
    }
    Ok(gpm)
}

// #[cfg_attr(feature = "axstd", no_mangle)]
// fn main_prev() {
//     println!("Starting virtualization...");
//     info!("Hardware support: {:?}", axvm::has_hardware_support());

//     let percpu = unsafe { AXVM_PER_CPU.current_ref_mut_raw() };
//     percpu.init(0).expect("Failed to initialize percpu state");
//     percpu
//         .hardware_enable()
//         .expect("Failed to enable virtualization");

//     let gpm = setup_gpm().expect("Failed to set guest physical memory set");
//     debug!("{:#x?}", gpm);
//     let mut vcpu = percpu
//         .create_vcpu(GUEST_ENTRY, gpm.nest_page_table_root())
//         .expect("Failed to create vcpu");

//     debug!("{:#x?}", vcpu);

//     println!("Running guest...");

//     vcpu.run();
// }

#[percpu::def_percpu]
pub static mut AXVM_PER_CPU: AxVMPerCpu<AxVMHalImpl> = AxVMPerCpu::new_uninit();

#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    let config = AxVMConfig {
        cpu_count: 1,
        cpu_config: AxVCpuConfig {
            arch_config: AxArchVCpuConfig::default(),
            ap_entry:GUEST_ENTRY,
            bsp_entry: GUEST_ENTRY,
        },
    };

    unsafe {
        let percpu = AXVM_PER_CPU.current_ref_mut_raw();
        // cpu id todo
        percpu.init(0).expect("Failed to initialize percpu state");
        percpu
            .hardware_enable()
            .expect("Failed to enable virtualization");
    }
    let gpm = setup_gpm().expect("Failed to set guest physical memory set");
    let vm = AxVM::<AxVMHalImpl>::new(config, 0, gpm.nest_page_table_root()).expect("Failed to create VM");
    vm.boot().unwrap()
}

use axerrno::{AxError, AxResult};
use axvm::{GuestPhysAddr, HostPhysAddr, HostVirtAddr};
use axhal::mem::virt_to_phys;

use crate::{GuestPhysMemorySet, GuestMemoryRegion, MappingFlags};

pub const GUEST_PHYS_MEMORY_BASE: GuestPhysAddr = 0;
pub const BIOS_ENTRY: GuestPhysAddr = 0x8000;
pub const GUEST_ENTRY: GuestPhysAddr = 0x20_0000;
pub const GUEST_PHYS_MEMORY_SIZE: usize = 0x100_0000; // 16M

#[repr(align(4096))]
struct AlignedMemory<const LEN: usize>([u8; LEN]);

static mut GUEST_PHYS_MEMORY: AlignedMemory<GUEST_PHYS_MEMORY_SIZE> =
    AlignedMemory([0; GUEST_PHYS_MEMORY_SIZE]);

fn gpa_as_mut_ptr(guest_paddr: GuestPhysAddr) -> *mut u8 {
    let offset = unsafe { core::ptr::addr_of!(GUEST_PHYS_MEMORY) as *const _ as usize };
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

pub fn setup_gpm() -> AxResult<GuestPhysMemorySet> {
    // copy BIOS and guest images from file system
    load_guest_image_from_file_system("rvm-bios.bin", BIOS_ENTRY)?;
    load_guest_image_from_file_system("nimbos.bin", GUEST_ENTRY)?;

    // create nested page table and add mapping
    let mut gpm = GuestPhysMemorySet::new()?;
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
    for r in guest_memory_regions.into_iter() {
        trace!("{:#x?}", r);
        gpm.map_region(r.into())?;
    }
    Ok(gpm)
}

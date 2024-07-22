mod dtb_aarch64;

use self::dtb_aarch64::MachineMeta;
use axalloc::GlobalPage;
use axerrno::{AxError, AxResult};
use axhal::mem::virt_to_phys;
use axvm::{GuestPhysAddr, HostPhysAddr, HostVirtAddr};
use page_table_entry::MappingFlags;

use crate::gpm::{GuestMemoryRegion, GuestPhysMemorySet};
use alloc::vec;
use alloc::vec::Vec;

pub const GUEST_PHYS_MEMORY_BASE: GuestPhysAddr = 0x4000_0000;
pub const DTB_ENTRY: GuestPhysAddr = 0x0;
pub const GUEST_ENTRY: GuestPhysAddr = 0x4008_0000;
pub const GUEST_PHYS_MEMORY_SIZE: usize = 0x100_0000; // 16M

#[repr(align(4096))]
struct AlignedMemory<const LEN: usize>([u8; LEN]);

static mut GUEST_PHYS_MEMORY: AlignedMemory<GUEST_PHYS_MEMORY_SIZE> =
    AlignedMemory([0; GUEST_PHYS_MEMORY_SIZE]);

pub fn gpa_as_mut_ptr(guest_paddr: GuestPhysAddr) -> *mut u8 {
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
    // debug!("Loading {} to GPA {:#x} gpa_ptr_addr:{:#x}", file_name, load_gpa, gpa_as_mut_ptr(load_gpa) as usize);
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
    load_guest_image_from_file_system("nimbos.dtb", DTB_ENTRY)?;
    load_guest_image_from_file_system("nimbos.bin", GUEST_ENTRY)?;

    let mut guest_memory_regions: Vec<GuestMemoryRegion> = vec![];
    let mut gpm = GuestPhysMemorySet::new()?;
    let meta = MachineMeta::parse(DTB_ENTRY);
    // RAM
    guest_memory_regions.push(GuestMemoryRegion {
        gpa: GUEST_PHYS_MEMORY_BASE,
        hpa: virt_to_phys(HostVirtAddr::from(
            gpa_as_mut_ptr(GUEST_PHYS_MEMORY_BASE) as usize
        )),
        size: GUEST_PHYS_MEMORY_SIZE,
        flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE,
    });
    // hardcode for virtio
    guest_memory_regions.push(GuestMemoryRegion {
        gpa: 0x0a00_0000,
        hpa: HostPhysAddr::from(0x0a00_0000),
        size: 0x4000,
        flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
    });
    // uart
    if let Some(pl011) = meta.pl011 {
        guest_memory_regions.push(GuestMemoryRegion {
            gpa: pl011.base_address as GuestPhysAddr,
            hpa: HostPhysAddr::from(pl011.base_address),
            size: pl011.size,
            flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        });
    }
    // timer
    if let Some(pl031) = meta.pl031 {
        guest_memory_regions.push(GuestMemoryRegion {
            gpa: pl031.base_address as GuestPhysAddr,
            hpa: HostPhysAddr::from(pl031.base_address),
            size: pl031.size,
            flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        });
    }
    // rtc
    if let Some(pl061) = meta.pl061 {
        guest_memory_regions.push(GuestMemoryRegion {
            gpa: pl061.base_address as GuestPhysAddr,
            hpa: HostPhysAddr::from(pl061.base_address),
            size: pl061.size,
            flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        });
    }
    // gic
    for intc in meta.intc.iter() {
        guest_memory_regions.push(GuestMemoryRegion {
            gpa: intc.base_address as GuestPhysAddr,
            hpa: HostPhysAddr::from(intc.base_address),
            size: intc.size,
            flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        });
    }
    // pcie
    if let Some(pcie) = meta.pcie {
        guest_memory_regions.push(GuestMemoryRegion {
            gpa: pcie.base_address as GuestPhysAddr,
            hpa: HostPhysAddr::from(pcie.base_address),
            size: pcie.size,
            flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        });
    }
    // map all regions
    for r in guest_memory_regions.into_iter() {
        trace!("{:#x?}", r);
        gpm.map_region(r.into())?;
    }
    Ok(gpm)
}

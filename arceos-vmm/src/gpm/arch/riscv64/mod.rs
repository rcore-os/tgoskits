mod dtb_riscv64;

use self::dtb_riscv64::MachineMeta;
use axalloc::GlobalPage;
use axerrno::{AxError, AxResult};
use axhal::mem::virt_to_phys;
use axvm::{GuestPhysAddr, HostPhysAddr, HostVirtAddr};
use page_table_entry::MappingFlags;

use crate::gpm::{GuestMemoryRegion, GuestPhysMemorySet};

pub const GUEST_PHYS_MEMORY_BASE: GuestPhysAddr = 0x9000_0000;
pub const DTB_ENTRY: GuestPhysAddr = 0x9000_0000;
pub const GUEST_ENTRY: GuestPhysAddr = 0x9020_0000;
pub const GUEST_PHYS_MEMORY_SIZE: usize = 0x400_0000; // 64M

#[repr(align(4096))]
struct AlignedMemory<const LEN: usize>([u8; LEN]);

static mut GUEST_PHYS_MEMORY: AlignedMemory<GUEST_PHYS_MEMORY_SIZE> =
    AlignedMemory([0; GUEST_PHYS_MEMORY_SIZE]);

// TODO:need use gpm to transfer
fn gpa_as_mut_ptr(guest_paddr: GuestPhysAddr) -> *mut u8 {
    let offset = unsafe { core::ptr::addr_of!(GUEST_PHYS_MEMORY) as *const _ as usize };
    let host_vaddr = guest_paddr + offset - GUEST_PHYS_MEMORY_BASE;
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
    load_guest_image_from_file_system("nimbos.dtb", DTB_ENTRY)?;
    load_guest_image_from_file_system("nimbos.bin", GUEST_ENTRY)?;

    let mut gpm = GuestPhysMemorySet::new()?;
    let meta = MachineMeta::parse(gpa_as_mut_ptr(DTB_ENTRY) as usize);
    if let Some(test) = meta.test_finisher_address {
        gpm.map_region(
            GuestMemoryRegion {
                gpa: test.base_address,
                hpa: test.base_address.into(),
                size: test.size + 0x1000,
                flags: MappingFlags::READ
                    | MappingFlags::WRITE
                    | MappingFlags::USER
                    | MappingFlags::EXECUTE,
            }
            .into(),
        )?;
    }
    for virtio in meta.virtio.iter() {
        gpm.map_region(
            GuestMemoryRegion {
                gpa: virtio.base_address,
                hpa: virtio.base_address.into(),
                size: virtio.size,
                flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
            }
            .into(),
        )?;
    }

    if let Some(uart) = meta.uart {
        gpm.map_region(
            GuestMemoryRegion {
                gpa: uart.base_address,
                hpa: uart.base_address.into(),
                size: 0x1000,
                flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
            }
            .into(),
        )?;
    }

    if let Some(clint) = meta.clint {
        gpm.map_region(
            GuestMemoryRegion {
                gpa: clint.base_address,
                hpa: clint.base_address.into(),
                size: clint.size,
                flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
            }
            .into(),
        )?;
    }

    if let Some(plic) = meta.plic {
        gpm.map_region(
            GuestMemoryRegion {
                gpa: plic.base_address,
                hpa: plic.base_address.into(),
                size: 0x20_0000,
                flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
            }
            .into(),
        )?;
    }

    if let Some(pci) = meta.pci {
        gpm.map_region(
            GuestMemoryRegion {
                gpa: pci.base_address,
                hpa: pci.base_address.into(),
                size: pci.size,
                flags: MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
            }
            .into(),
        )?;
    }

    info!(
        "physical memory: [{:#x}: {:#x})",
        meta.physical_memory_offset,
        meta.physical_memory_offset + meta.physical_memory_size
    );

    gpm.map_region(
        GuestMemoryRegion {
            gpa: meta.physical_memory_offset,
            hpa: virt_to_phys(HostVirtAddr::from(
                gpa_as_mut_ptr(GUEST_PHYS_MEMORY_BASE) as usize
            )),
            size: meta.physical_memory_size,
            flags: MappingFlags::READ
                | MappingFlags::WRITE
                | MappingFlags::EXECUTE
                | MappingFlags::USER,
        }
        .into(),
    )?;

    Ok(gpm)
}

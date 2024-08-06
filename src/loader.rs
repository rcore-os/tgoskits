//! Loader for loading apps.
//!
//! It will read and parse ELF files.
//!
//! Now these apps are loaded into memory as a part of the kernel image.
use core::arch::global_asm;

use alloc::vec::Vec;
use axhal::paging::MappingFlags;
use memory_addr::VirtAddr;

global_asm!(include_str!(concat!(env!("OUT_DIR"), "/link_app.S")));

extern "C" {
    fn _app_count();
}

/// Get the number of apps.
pub(crate) fn get_app_count() -> usize {
    unsafe { (_app_count as *const u64).read() as usize }
}

/// Get the name of an app by a given app ID.
pub(crate) fn get_app_name(app_id: usize) -> &'static str {
    unsafe {
        let app_0_start_ptr = (_app_count as *const u64).add(1);
        assert!(app_id < get_app_count());
        let app_name = app_0_start_ptr.add(app_id * 2).read() as *const u8;
        let mut len = 0;
        while app_name.add(len).read() != b'\0' {
            len += 1;
        }
        let slice = core::slice::from_raw_parts(app_name, len);
        core::str::from_utf8(slice).unwrap()
    }
}

/// Get the data of an app by a given app ID.
pub(crate) fn get_app_data(app_id: usize) -> &'static [u8] {
    unsafe {
        let app_0_start_ptr = (_app_count as *const u64).add(1);
        assert!(app_id < get_app_count());
        let app_start = app_0_start_ptr.add(app_id * 2 + 1).read() as usize;
        let app_end = app_0_start_ptr.add(app_id * 2 + 2).read() as usize;
        let app_size = app_end - app_start;
        core::slice::from_raw_parts(app_start as *const u8, app_size)
    }
}

/// Get the data of an app by the given app name.
pub(crate) fn get_app_data_by_name(name: &str) -> Option<&'static [u8]> {
    let app_count = get_app_count();
    (0..app_count)
        .find(|&i| get_app_name(i) == name)
        .map(get_app_data)
}

/// List all apps.
pub(crate) fn list_apps() {
    info!("/**** APPS ****");
    let app_count = get_app_count();
    for i in 0..app_count {
        info!("{}", get_app_name(i));
    }
    info!("**************/");
}

/// The segment of the elf file, which is used to map the elf file to the memory space
pub struct ELFSegment {
    /// The start virtual address of the segment
    pub start_vaddr: VirtAddr,
    /// The size of the segment
    pub size: usize,
    /// The flags of the segment which is used to set the page table entry
    pub flags: MappingFlags,
    /// The data of the segment
    pub data: &'static [u8],
    /// The offset of the segment relative to the start of the page
    pub offset: usize,
}

/// The information of a given ELF file
pub struct ELFInfo {
    /// The entry point of the ELF file
    pub entry: VirtAddr,
    /// The segments of the ELF file
    pub segments: Vec<ELFSegment>,
}

/// Load the ELF files by the given app name and return
/// the segments of the ELF file
///
/// # Arguments
/// * `name` - The name of the app
///
/// # Returns
/// Entry and information about segments of the given ELF file
pub(crate) fn load_user_app(name: &str) -> ELFInfo {
    use xmas_elf::program::{Flags, SegmentData};
    use xmas_elf::{header, ElfFile};

    let elf = ElfFile::new(
        get_app_data_by_name(name).unwrap_or_else(|| panic!("failed to get app: {}", name)),
    )
    .expect("invalid ELF file");
    let elf_header = elf.header;

    let elf_magic_number = elf_header.pt1.magic;

    assert_eq!(elf_magic_number, *b"\x7fELF", "invalid elf!");

    assert_eq!(
        elf.header.pt2.type_().as_type(),
        header::Type::Executable,
        "ELF is not an executable object"
    );

    let expect_arch = if cfg!(target_arch = "x86_64") {
        header::Machine::X86_64
    } else if cfg!(target_arch = "aarch64") {
        header::Machine::AArch64
    } else if cfg!(target_arch = "riscv64") {
        header::Machine::RISC_V
    } else {
        panic!("Unsupported architecture!");
    };
    assert_eq!(
        elf.header.pt2.machine().as_machine(),
        expect_arch,
        "invalid ELF arch"
    );

    fn into_mapflag(f: Flags) -> MappingFlags {
        let mut ret = MappingFlags::USER;
        if f.is_read() {
            ret |= MappingFlags::READ;
        }
        if f.is_write() {
            ret |= MappingFlags::WRITE;
        }
        if f.is_execute() {
            ret |= MappingFlags::EXECUTE;
        }
        ret
    }

    let mut segments = Vec::new();
    elf.program_iter()
        .filter(|ph| ph.get_type() == Ok(xmas_elf::program::Type::Load))
        .for_each(|ph| {
            // align the segment to 4k
            let st_vaddr = VirtAddr::from(ph.virtual_addr() as usize);
            let st_vaddr_align: VirtAddr = st_vaddr.align_down_4k();
            let ed_vaddr_align =
                VirtAddr::from((ph.virtual_addr() + ph.mem_size()) as usize).align_up_4k();
            let data = match ph.get_data(&elf).unwrap() {
                SegmentData::Undefined(data) => data,
                _ => panic!("failed to get ELF segment data"),
            };
            segments.push(ELFSegment {
                start_vaddr: st_vaddr_align,
                size: ed_vaddr_align.as_usize() - st_vaddr_align.as_usize(),
                flags: into_mapflag(ph.flags()),
                data,
                offset: st_vaddr.align_offset_4k(),
            });
        });
    ELFInfo {
        entry: VirtAddr::from(elf.header.pt2.entry_point() as usize),
        segments,
    }
}

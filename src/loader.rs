//! Loader for loading apps.
//!
//! It will read and parse ELF files.
//!
//! Now these apps are loaded into memory as a part of the kernel image.
use alloc::{boxed::Box, collections::btree_map::BTreeMap, vec::Vec};
//use core::arch::global_asm;

use axhal::paging::MappingFlags;
use memory_addr::{MemoryAddr, VirtAddr};

// /// The segment of the elf file, which is used to map the elf file to the memory space
// pub struct ELFSegment {
//     /// The start virtual address of the segment
//     pub start_vaddr: VirtAddr,
//     /// The size of the segment
//     pub size: usize,
//     /// The flags of the segment which is used to set the page table entry
//     pub flags: MappingFlags,
//     /// The data of the segment
//     pub data: &'static [u8],
//     /// The offset of the segment relative to the start of the page
//     pub offset: usize,
// }

// /// The information of a given ELF file
// pub struct ELFInfo {
//     /// The entry point of the ELF file
//     pub entry: VirtAddr,
//     /// The segments of the ELF file
//     pub segments: Vec<ELFSegment>,
//     /// The auxiliary vectors of the ELF file
//     pub auxv: BTreeMap<u8, usize>,
// }

/// Load the ELF files by the given app name and return
/// the segments of the ELF file
///
/// # Arguments
/// * `name` - The name of the app
/// * `base_addr` - The minimal address of user space
///
/// # Returns
/// Entry and information about segments of the given ELF file
pub(crate) fn load_elf(name: &str, base_addr: VirtAddr) -> ELFInfo {
    use xmas_elf::program::{Flags, SegmentData};
    use xmas_elf::{ElfFile, header};

    let path = axfs::api::read(name).unwrap();
    let path_slice = Box::leak(path.into_boxed_slice());
    let elf = ElfFile::new(path_slice).expect("invalid ELF file");
    let elf_header = elf.header;

    assert_eq!(elf_header.pt1.magic, *b"\x7fELF", "invalid elf!");

    let expect_arch = if cfg!(target_arch = "x86_64") {
        header::Machine::X86_64
    } else if cfg!(target_arch = "aarch64") {
        header::Machine::AArch64
    } else if cfg!(target_arch = "riscv64") {
        header::Machine::RISC_V
    } else if cfg!(target_arch = "loongarch64") {
        header::Machine::Other(0x102)
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
            let st_vaddr = VirtAddr::from(ph.virtual_addr() as usize) + elf_offset;
            let st_vaddr_align: VirtAddr = st_vaddr.align_down_4k();
            let ed_vaddr_align = VirtAddr::from((ph.virtual_addr() + ph.mem_size()) as usize)
                .align_up_4k()
                + elf_offset;
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
        entry: VirtAddr::from(elf.header.pt2.entry_point() as usize + elf_offset),
        segments,
        auxv: kernel_elf_parser::get_auxv_vector(&elf, elf_offset),
    }
}

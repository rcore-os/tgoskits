use core::str::from_utf8;

use alloc::{collections::vec_deque::VecDeque, string::String, vec};

use axerrno::{AxError, AxResult};
use axhal::{
    paging::MappingFlags,
    trap::{PAGE_FAULT, register_trap_handler},
};

use axmm::AddrSpace;
use axtask::TaskExtRef;
use kernel_elf_parser::{AuxvEntry, ELFParser, app_stack_region};
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use xmas_elf::{ElfFile, program::SegmentData};

/// Map the elf file to the user address space.
///
/// # Arguments
/// - `args`: The arguments of the user app. The first argument is the path of the user app.
/// - `elf_parser`: The parser of the elf file.
/// - `uspace`: The address space of the user app.
///
/// # Returns
/// - The entry point of the user app.
fn map_elf(
    args: &mut VecDeque<String>,
    elf_parser: &ELFParser,
    uspace: &mut AddrSpace,
) -> AxResult<(VirtAddr, [AuxvEntry; 17])> {
    let elf = elf_parser.elf();
    if let Some(interp) = elf
        .program_iter()
        .find(|ph| ph.get_type() == Ok(xmas_elf::program::Type::Interp))
    {
        let interp = match interp.get_data(elf) {
            Ok(SegmentData::Undefined(data)) => data,
            _ => panic!("Invalid data in Interp Elf Program Header"),
        };

        let interp_path = from_utf8(interp).map_err(|_| AxError::InvalidInput)?;
        // remove trailing '\0'
        let mut real_interp_path =
            axfs::api::canonicalize(interp_path.trim_matches(char::from(0)))?;
        if real_interp_path == "/lib/ld-linux-riscv64-lp64.so.1"
            || real_interp_path == "/lib64/ld-linux-loongarch-lp64d.so.1"
        {
            // TODO: Use soft link
            real_interp_path = String::from("./musl/lib/libc.so");
        }

        let interp_data = axfs::api::read(real_interp_path.as_str())?;
        let interp_elf = ElfFile::new(&interp_data).map_err(|_| AxError::InvalidData)?;
        let uspace_base = uspace.base().as_usize();

        let interp_elf_parser = ELFParser::new(
            &interp_elf,
            axconfig::plat::USER_INTERP_BASE,
            Some(uspace_base as isize),
            uspace_base,
        )
        .map_err(|_| AxError::InvalidData)?;
        // Set the first argument to the path of the user app.
        args.push_front(real_interp_path);
        return map_elf(args, &interp_elf_parser, uspace);
    }
    for segement in elf_parser.ph_load() {
        debug!(
            "Mapping ELF segment: [{:#x?}, {:#x?}) flags: {:#x?}",
            segement.vaddr,
            segement.vaddr + segement.memsz as usize,
            segement.flags
        );
        let seg_pad = segement.vaddr.align_offset_4k();
        assert_eq!(seg_pad, segement.offset % PAGE_SIZE_4K);

        let seg_align_size =
            (segement.memsz as usize + seg_pad + PAGE_SIZE_4K - 1) & !(PAGE_SIZE_4K - 1);
        uspace.map_alloc(
            segement.vaddr.align_down_4k(),
            seg_align_size,
            segement.flags,
            true,
        )?;
        let seg_data = elf
            .input
            .get(segement.offset..segement.offset + segement.filesz as usize)
            .ok_or(AxError::InvalidData)?;
        uspace.write(segement.vaddr, seg_data)?;
        // TDOO: flush the I-cache
    }

    Ok((
        elf_parser.entry().into(),
        elf_parser.auxv_vector(PAGE_SIZE_4K),
    ))
}

/// Load the user app to the user address space.
///
/// # Arguments
/// - `args`: The arguments of the user app. The first argument is the path of the user app.
/// - `uspace`: The address space of the user app.
///
/// # Returns
/// - The entry point of the user app.
/// - The stack pointer of the user app.
pub fn load_user_app(
    args: &mut VecDeque<String>,
    uspace: &mut AddrSpace,
) -> AxResult<(VirtAddr, VirtAddr)> {
    if args.is_empty() {
        return Err(AxError::InvalidInput);
    }
    let file_data = axfs::api::read(args[0].as_str())?;
    let elf = ElfFile::new(&file_data).map_err(|_| AxError::InvalidData)?;

    let uspace_base = uspace.base().as_usize();
    let elf_parser = ELFParser::new(
        &elf,
        axconfig::plat::USER_INTERP_BASE,
        Some(uspace_base as isize),
        uspace_base,
    )
    .map_err(|_| AxError::InvalidData)?;

    let (entry, mut auxv) = map_elf(args, &elf_parser, uspace)?;
    // The user stack is divided into two parts:
    // `ustack_start` -> `ustack_pointer`: It is the stack space that users actually read and write.
    // `ustack_pointer` -> `ustack_end`: It is the space that contains the arguments, environment variables and auxv passed to the app.
    //  When the app starts running, the stack pointer points to `ustack_pointer`.
    let ustack_end = VirtAddr::from_usize(axconfig::plat::USER_STACK_TOP);
    let ustack_size = axconfig::plat::USER_STACK_SIZE;
    let ustack_start = ustack_end - ustack_size;
    debug!(
        "Mapping user stack: {:#x?} -> {:#x?}",
        ustack_start, ustack_end
    );
    // FIXME: Add more arguments and environment variables
    let env = vec![
        "SHLVL=1".into(),
        "PWD=/".into(),
        "GCC_EXEC_PREFIX=/riscv64-linux-musl-native/bin/../lib/gcc/".into(),
        "COLLECT_GCC=./riscv64-linux-musl-native/bin/riscv64-linux-musl-gcc".into(),
        "COLLECT_LTO_WRAPPER=/riscv64-linux-musl-native/bin/../libexec/gcc/riscv64-linux-musl/11.2.1/lto-wrapper".into(),
        "COLLECT_GCC_OPTIONS='-march=rv64gc' '-mabi=lp64d' '-march=rv64imafdc' '-dumpdir' 'a.'".into(),
        "LIBRARY_PATH=/lib/".into(),
        "LD_LIBRARY_PATH=/lib/".into(),
        "LD_DEBUG=files".into(),
    ];

    let stack_data = app_stack_region(
        args.make_contiguous(),
        &env,
        &mut auxv,
        ustack_start,
        ustack_size,
    );
    uspace.map_alloc(
        ustack_start,
        ustack_size,
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        true,
    )?;

    let user_sp = ustack_end - stack_data.len();

    uspace.write(user_sp, stack_data.as_slice())?;

    Ok((entry, user_sp))
}

#[register_trap_handler(PAGE_FAULT)]
fn handle_page_fault(vaddr: VirtAddr, access_flags: MappingFlags, is_user: bool) -> bool {
    if is_user {
        if !axtask::current()
            .task_ext()
            .aspace
            .lock()
            .handle_page_fault(vaddr, access_flags)
        {
            warn!(
                "{}: segmentation fault at {:#x}, exit!",
                axtask::current().id_name(),
                vaddr
            );
            axtask::exit(-1);
        }
        true
    } else {
        false
    }
}

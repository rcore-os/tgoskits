//! Native LoongArch UEFI PE entry and its pre-Rust relocation shim.

use core::{
    arch::{asm, naked_asm},
    ffi::c_void,
};

use uefi::{Handle, Status};

const IMAGE_DOS_SIGNATURE: u32 = 0x5a4d;
const LINUX_PE_MAGIC: u32 = 0x8182_23cd;
const IMAGE_NT_SIGNATURE: u32 = 0x0000_4550;
const IMAGE_FILE_MACHINE_LOONGARCH64: u16 = 0x6264;
const IMAGE_FILE_DEBUG_STRIPPED: u16 = 0x0200;
const IMAGE_FILE_EXECUTABLE_IMAGE: u16 = 0x0002;
const IMAGE_FILE_LINE_NUMS_STRIPPED: u16 = 0x0004;
const IMAGE_FILE_CHARACTERISTICS: u16 =
    IMAGE_FILE_DEBUG_STRIPPED | IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_LINE_NUMS_STRIPPED;
const IMAGE_SUBSYSTEM_EFI_APPLICATION: u16 = 10;
const PE_DATA_SECTION_CHARACTERISTICS: u32 = 0xc000_0040;
const PE_TEXT_SECTION_CHARACTERISTICS: u32 = 0x6000_0020;
const R_LARCH_RELATIVE: u32 = 3;
const RELA_SIZE: usize = 24;
const UEFI_RESET_COLD: u32 = 0;
const UEFI_STATUS_LOAD_ERROR: usize = 0x8000_0000_0000_0001;
const SYSTEM_TABLE_RUNTIME_SERVICES: usize = 88;
const RUNTIME_SERVICES_RESET_SYSTEM: usize = 104;

unsafe extern "C" {
    fn __bss_start();
    fn __bss_stop();
    fn __rela_dyn_begin();
    fn __rela_dyn_end();
}

unsafe extern "efiapi" {
    fn efi_main(image_handle: Handle, system_table: *const c_void) -> Status;
}

macro_rules! running_symbol {
    ($symbol:expr) => {{
        let address: usize;
        // SAFETY: this computes a PC-relative address without reading memory
        // or changing the stack.
        unsafe {
            asm!(
                concat!("la.pcrel {address}, ", stringify!($symbol)),
                address = out(reg) address,
                options(nostack),
            );
        }
        address
    }};
}

#[repr(C)]
struct Rela {
    offset: u64,
    info: u64,
    addend: i64,
}

fn relocate(system_table: *const c_void) {
    let load_offset = running_symbol!(_head) as i128;
    let start = running_symbol!(__rela_dyn_begin);
    let end = running_symbol!(__rela_dyn_end);
    let length = end.saturating_sub(start);
    if length % RELA_SIZE != 0 {
        reset_after_relocation_error(system_table);
    }
    for offset in (0..length).step_by(RELA_SIZE) {
        // SAFETY: the linker places the relocation table inside a PT_LOAD
        // section at the running address returned above.
        let rela = unsafe { &*((start + offset) as *const Rela) };
        let relocation_type = rela.info as u32;
        let symbol_index = (rela.info >> 32) as u32;
        if relocation_type != R_LARCH_RELATIVE || symbol_index != 0 {
            reset_after_relocation_error(system_table);
        }
        let destination = ((rela.offset as i128) + load_offset) as usize as *mut usize;
        let value = ((rela.addend as i128) + load_offset) as usize;
        // SAFETY: the linker emitted this relative relocation for a word in
        // the loaded image, and the PE loader placed that whole image at
        // `load_offset`.
        unsafe { destination.write_volatile(value) };
    }
    // Publish relocated data and synchronize the instruction stream before
    // entering any Rust code.
    // SAFETY: the barriers have no operands and do not touch the stack.
    unsafe {
        asm!("dbar 0", "ibar 0", options(nostack));
    }
}

fn reset_after_relocation_error(system_table: *const c_void) -> ! {
    if !system_table.is_null() {
        // SAFETY: UEFI supplies the system table for the complete entry call.
        // The fixed offsets are from EFI_SYSTEM_TABLE and
        // EFI_RUNTIME_SERVICES; pointer fields are read unaligned because the
        // firmware only guarantees its own table alignment.
        let runtime_services = unsafe {
            core::ptr::read_unaligned(
                system_table
                    .cast::<u8>()
                    .add(SYSTEM_TABLE_RUNTIME_SERVICES)
                    .cast::<*mut c_void>(),
            )
        };
        if !runtime_services.is_null() {
            // SAFETY: the RuntimeServices table and the ResetSystem entry are
            // firmware-owned and valid while Boot Services are active.
            let reset_address = unsafe {
                core::ptr::read_unaligned(
                    runtime_services
                        .cast::<u8>()
                        .add(RUNTIME_SERVICES_RESET_SYSTEM)
                        .cast::<usize>(),
                )
            };
            if reset_address != 0 {
                // SAFETY: EFI_RUNTIME_SERVICES.ResetSystem has this UEFI ABI.
                let reset: unsafe extern "efiapi" fn(u32, usize, usize, *mut c_void) = unsafe {
                    core::mem::transmute::<
                        usize,
                        unsafe extern "efiapi" fn(u32, usize, usize, *mut c_void),
                    >(reset_address)
                };
                // SAFETY: all arguments follow EFI_RESET_SYSTEM's contract.
                unsafe {
                    reset(
                        UEFI_RESET_COLD,
                        UEFI_STATUS_LOAD_ERROR,
                        0,
                        core::ptr::null_mut(),
                    );
                }
            }
        }
    }
    // SAFETY: a relocation failure is unrecoverable. If firmware reset is not
    // available, raise an architecture break exception instead of spinning.
    unsafe { asm!("break 0", options(noreturn)) }
}

unsafe extern "efiapi" fn rust_entry(image_handle: Handle, system_table: *const c_void) -> Status {
    // SAFETY: `efi_main` is the function exported by `uefi::entry`; this shim
    // supplies the firmware values after relocation and BSS initialization.
    unsafe { efi_main(image_handle, system_table) }
}

/// UEFI firmware entry after the PE loader has placed the image at any address.
///
/// # Safety
///
/// UEFI firmware must call this with a valid image handle and system table.
/// The image must contain the linker-provided BSS and relocation bounds.
#[unsafe(naked)]
#[unsafe(export_name = "efi_pe_entry")]
#[unsafe(link_section = ".text")]
pub unsafe extern "efiapi" fn efi_pe_entry(
    _image_handle: Handle,
    _system_table: *const c_void,
) -> Status {
    naked_asm!(
        "
        move        $t0, $sp
        bstrins.d   $sp, $zero, 3, 0
        addi.d      $sp, $sp, -32
        st.d        $ra, $sp, 0
        st.d        $a0, $sp, 8
        st.d        $a1, $sp, 16
        st.d        $t0, $sp, 24

        la.pcrel    $t0, {bss_start}
        la.pcrel    $t1, {bss_stop}
    1:
        bgeu        $t0, $t1, 2f
        st.d        $zero, $t0, 0
        addi.d      $t0, $t0, 8
        b           1b
    2:
        dbar        0

        ld.d        $a0, $sp, 16
        ld.d        $a1, $sp, 16
        bl          {relocate}
        ld.d        $a0, $sp, 8
        ld.d        $a1, $sp, 16
        bl          {entry}

        ld.d        $t0, $sp, 24
        ld.d        $ra, $sp, 0
        move        $sp, $t0
        ret
        ",
        bss_start = sym __bss_start,
        bss_stop = sym __bss_stop,
        relocate = sym relocate,
        entry = sym rust_entry,
    )
}

/// Minimal PE32+ header for a LoongArch EFI application based at zero.
///
/// # Safety
///
/// This symbol is image data consumed by the firmware PE loader. Rust code
/// must not call it.
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".head.text")]
pub unsafe extern "C" fn _head() {
    naked_asm!(
        ".word {dos_signature}",
        ".org 0x38",
        ".long {linux_pe_magic}",
        ".long 4f - _head",

        "4:",
        ".long {nt_signature}",
        ".short {machine}",
        ".short 2",
        ".long 0",
        ".long 0",
        ".long 0",
        ".short 2f - 1f",
        ".short {file_flags}",

        "1:",
        ".short 0x020b",
        ".byte 0x02",
        ".byte 0x14",
        ".long _etext - _stext",
        ".long _kernel_rsize",
        ".long _kernel_vsize - _kernel_rsize",
        ".long {efi_entry} - _head",
        ".long _stext - _head",
        ".quad 0",
        ".long 0x1000",
        ".long 0x200",
        ".short 0",
        ".short 0",
        ".short 0",
        ".short 0",
        ".short 0",
        ".short 0",
        ".long 0",
        ".long _end - _head",
        ".long _stext - _head",
        ".long 0",
        ".short {subsystem}",
        ".short 0",
        ".quad 0",
        ".quad 0",
        ".quad 0",
        ".quad 0",
        ".long 0",
        ".long (2f - .) / 8",
        ".quad 0",
        ".quad 0",
        ".quad 0",
        ".quad 0",
        ".quad 0",
        ".quad 0",

        "2:",
        ".ascii \".text\\0\\0\\0\"",
        ".long _etext - _stext",
        ".long _stext - _head",
        ".long _etext - _stext",
        ".long _stext - _head",
        ".long 0",
        ".long 0",
        ".short 0",
        ".short 0",
        ".long {text_characteristics}",

        ".ascii \".data\\0\\0\\0\"",
        ".long _kernel_vsize",
        ".long _etext - _head",
        ".long _kernel_rsize",
        ".long _etext - _head",
        ".long 0",
        ".long 0",
        ".short 0",
        ".short 0",
        ".long {data_characteristics}",
        dos_signature = const IMAGE_DOS_SIGNATURE,
        linux_pe_magic = const LINUX_PE_MAGIC,
        nt_signature = const IMAGE_NT_SIGNATURE,
        machine = const IMAGE_FILE_MACHINE_LOONGARCH64,
        file_flags = const IMAGE_FILE_CHARACTERISTICS,
        efi_entry = sym efi_pe_entry,
        subsystem = const IMAGE_SUBSYSTEM_EFI_APPLICATION,
        text_characteristics = const PE_TEXT_SECTION_CHARACTERISTICS,
        data_characteristics = const PE_DATA_SECTION_CHARACTERISTICS,
    )
}

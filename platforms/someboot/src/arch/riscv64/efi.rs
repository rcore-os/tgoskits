//! RISC-V EFI entry and the firmware-owned boot-hart contract.

use core::{arch::naked_asm, ffi::c_void};

use uefi::{Handle, Status, StatusExt, boot, proto::unsafe_protocol, table};

/// Enters on a private image stack before touching relocatable Rust state.
///
/// # Safety
///
/// Firmware must call this on its boot hart in S-mode with valid EFI arguments,
/// identity-addressed EFI memory and a 16-byte-aligned stack. The complete ELF image,
/// including BSS and its private boot stack, must be writable and reserved.
#[unsafe(naked)]
#[unsafe(export_name = "efi_pe_entry")]
#[unsafe(link_section = ".text.efi_pe_entry")]
unsafe extern "efiapi" fn efi_entry(_image_handle: Handle, _system_table: *const c_void) -> Status {
    naked_asm!(
        ".option push",
        ".option norelax",
        // UEFI 2.11 permits identity-mapped firmware page tables. Retain them
        // through Boot Services and SetVirtualAddressMap; they are retired
        // before the early allocator can reuse Boot Services memory.
        // Retain the caller's context so a rejected firmware contract can
        // return an EFI error before taking over interrupts or page tables.
        "addi sp, sp, -64",
        "sd ra, 0(sp)",
        "sd gp, 8(sp)",
        "sd tp, 16(sp)",
        "sd s0, 24(sp)",
        "sd s1, 32(sp)",
        "sd s2, 40(sp)",
        // Firmware's timer vector may depend on its original scratch state.
        // Keep interrupts masked while replacing the boot CPU's context.
        "csrrci t0, sstatus, 2",
        "sd t0, 56(sp)",
        "csrr t0, sscratch",
        "sd t0, 48(sp)",
        "mv s2, sp",
        "mv s0, a0",
        "mv s1, a1",
        "lla gp, __global_pointer$",
        "mv tp, zero",
        "lla sp, __cpu0_stack_top",
        "addi sp, sp, -{boot_record_size}",
        // The live stack is part of BSS. Clear the two surrounding ranges,
        // using PC-relative symbols before the first relative relocation.
        "lla t0, __bss_start",
        "lla t1, __cpu0_stack",
        "2:",
        "bgeu t0, t1, 3f",
        "sb zero, 0(t0)",
        "addi t0, t0, 1",
        "j 2b",
        "3:",
        "lla t0, __cpu0_stack_top",
        "lla t1, __bss_stop",
        "4:",
        "bgeu t0, t1, 5f",
        "sb zero, 0(t0)",
        "addi t0, t0, 1",
        "j 4b",
        "5:",
        "call {relocate}",
        "mv a0, s0",
        "mv a1, s1",
        "call {efi_main}",
        // Success enters the kernel permanently. Only pre-takeover errors
        // return here, with firmware's stack and scratch ownership intact.
        "mv sp, s2",
        "ld t0, 48(sp)",
        "csrw sscratch, t0",
        "ld ra, 0(sp)",
        "ld gp, 8(sp)",
        "ld tp, 16(sp)",
        "ld s0, 24(sp)",
        "ld s1, 32(sp)",
        "ld s2, 40(sp)",
        "ld t0, 56(sp)",
        "addi sp, sp, 64",
        "csrw sstatus, t0",
        "ret",
        ".option pop",
        boot_record_size = const super::boot::STACK_SIZE,
        relocate = sym super::relocate::apply,
        efi_main = sym enter_after_relocation,
    );
}

/// Validates the firmware contract before committing the CPU boot record.
///
/// # Safety
///
/// The naked entry must have initialized BSS and relocations and reserved the
/// primary boot-record slot. EFI retains the supplied handle and system table.
unsafe extern "C" fn enter_after_relocation(
    image_handle: Handle,
    system_table: *const c_void,
) -> Status {
    // SAFETY: these are the original EFI entry arguments, still retained by
    // firmware. No Boot Services handle has been opened before initialization.
    unsafe {
        boot::set_image_handle(image_handle);
        table::set_system_table(system_table.cast());
    }
    let hart_id = match boot_hart_id() {
        Ok(hart_id) => hart_id,
        Err(error) => return error.status(),
    };
    let fdt = uefi::system::with_config_table(|tables| {
        tables
            .iter()
            .find(|entry| entry.guid == crate::efi_stub::FDT_TABLE_GUID)
            .map(|entry| entry.address as usize)
    });
    if !fdt.is_some_and(crate::fdt::set_fdt_addr_phys_if_valid) {
        return Status::NOT_FOUND;
    }
    // SAFETY: the wrapper reserves the final linker-stack slot, and neither
    // the boot record nor any CPU-local state has been published previously.
    unsafe { super::boot::initialize_efi_primary(hart_id) };
    // SAFETY: relocations, BSS and CPU identity are ready; the original EFI
    // arguments remain valid. This path never re-enters direct-boot clearing.
    unsafe { crate::efi_stub::efi_pe_entry_main(image_handle, system_table) }
}

#[derive(Debug)]
#[repr(C)]
#[unsafe_protocol("ccd15fec-6f73-4eec-8395-3e69e4b940bf")]
struct RiscvBootProtocol {
    revision: u64,
    get_boot_hart_id: unsafe extern "efiapi" fn(*mut Self, *mut usize) -> Status,
}

fn boot_hart_id() -> uefi::Result<usize> {
    let handle = boot::get_handle_for_protocol::<RiscvBootProtocol>()?;
    let mut protocol = boot::open_protocol_exclusive::<RiscvBootProtocol>(handle)?;
    if protocol.revision < 0x0001_0000 {
        return Err(Status::UNSUPPORTED.into());
    }
    let mut hart_id = 0;
    // SAFETY: the exclusive guard supplies the protocol with its specified
    // layout, the output is aligned and live for the call, and firmware does
    // not retain that pointer. The guard closes before leaving this function.
    unsafe { (protocol.get_boot_hart_id)(&mut *protocol, &mut hart_id) }.to_result()?;
    Ok(hart_id)
}

use core::{
    arch::asm,
    mem::{align_of, offset_of, size_of},
};

const ABI_MAGIC: u32 = u32::from_le_bytes(*b"AXBI");
const ABI_VERSION: u16 = 1;
const STACK_ALIGNMENT: usize = 16;

/// Versioned RISC-V CPU identity record shared by the physical and virtual
/// someboot entry paths.
///
/// Firmware's `a0` hart ID is captured here before shared Rust executes.
/// During early boot, `sscratch` points to this record. The platform binder
/// later replaces `sscratch` with the runtime CPU-area header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub(crate) struct CpuBootInfoV1 {
    abi_magic: u32,
    abi_version: u16,
    record_size: u16,
    hart_id: usize,
    cpu_meta_paddr: usize,
    early_trap_cause: usize,
    early_trap_pc: usize,
    early_trap_value: usize,
}

pub(crate) const ABI_MAGIC_VALUE: usize = ABI_MAGIC as usize;
pub(crate) const ABI_VERSION_VALUE: usize = ABI_VERSION as usize;
pub(crate) const RECORD_SIZE: usize = size_of::<CpuBootInfoV1>();
pub(crate) const STACK_SIZE: usize = (RECORD_SIZE + STACK_ALIGNMENT - 1) & !(STACK_ALIGNMENT - 1);

pub(crate) const ABI_MAGIC_OFFSET: usize = offset_of!(CpuBootInfoV1, abi_magic);
pub(crate) const ABI_VERSION_OFFSET: usize = offset_of!(CpuBootInfoV1, abi_version);
pub(crate) const RECORD_SIZE_OFFSET: usize = offset_of!(CpuBootInfoV1, record_size);
pub(crate) const HART_ID_OFFSET: usize = offset_of!(CpuBootInfoV1, hart_id);
pub(crate) const CPU_META_PADDR_OFFSET: usize = offset_of!(CpuBootInfoV1, cpu_meta_paddr);
pub(crate) const EARLY_TRAP_CAUSE_OFFSET: usize = offset_of!(CpuBootInfoV1, early_trap_cause);
pub(crate) const EARLY_TRAP_PC_OFFSET: usize = offset_of!(CpuBootInfoV1, early_trap_pc);
pub(crate) const EARLY_TRAP_VALUE_OFFSET: usize = offset_of!(CpuBootInfoV1, early_trap_value);

const _: () = {
    assert!(align_of::<CpuBootInfoV1>() <= STACK_ALIGNMENT);
    assert!(RECORD_SIZE <= u16::MAX as usize);
    assert!(STACK_SIZE <= 2047);
    assert!(ABI_MAGIC_OFFSET == 0);
    assert!(ABI_VERSION_OFFSET == 4);
    assert!(RECORD_SIZE_OFFSET == 6);
    assert!(HART_ID_OFFSET == 8);
    assert!(CPU_META_PADDR_OFFSET == 16);
    assert!(EARLY_TRAP_CAUSE_OFFSET == 24);
    assert!(EARLY_TRAP_PC_OFFSET == 32);
    assert!(EARLY_TRAP_VALUE_OFFSET == 40);
    assert!(RECORD_SIZE == 48);
    assert!(STACK_SIZE == 48);
};

impl CpuBootInfoV1 {
    pub(crate) const fn hart_id(self) -> usize {
        self.hart_id
    }

    pub(crate) const fn cpu_meta_paddr(self) -> usize {
        self.cpu_meta_paddr
    }

    const fn has_valid_header(self) -> bool {
        self.abi_magic == ABI_MAGIC
            && self.abi_version == ABI_VERSION
            && self.record_size as usize == RECORD_SIZE
    }
}

/// Reads and validates a boot record from an address that is mapped in the
/// current address space.
///
/// # Safety
///
/// `record_addr` must be non-null, aligned for [`CpuBootInfoV1`], readable for
/// the complete record, and must remain valid for the duration of this call.
pub(crate) unsafe fn read_at(record_addr: usize) -> CpuBootInfoV1 {
    assert_ne!(record_addr, 0, "RISC-V CPU boot record is null");
    assert_eq!(
        record_addr % align_of::<CpuBootInfoV1>(),
        0,
        "RISC-V CPU boot record is misaligned"
    );

    // SAFETY: the caller provides a readable, aligned boot-record address.
    let record = unsafe { (record_addr as *const CpuBootInfoV1).read() };
    assert!(
        record.has_valid_header(),
        "invalid RISC-V CPU boot record header"
    );
    record
}

/// Returns the current CPU's typed early-boot identity record.
///
/// This accessor is valid only before the platform binder changes `sscratch`
/// from [`CpuBootInfoV1`] to the runtime CPU-area header.
pub(crate) fn current() -> CpuBootInfoV1 {
    let record_addr: usize;
    unsafe {
        asm!(
            "csrr {record_addr}, sscratch",
            record_addr = out(reg) record_addr,
            options(nomem, nostack, preserves_flags)
        );

        // SAFETY: both someboot entry paths install a stack-resident record in
        // `sscratch` before entering shared Rust, and retain its stack slot
        // across the physical-to-virtual transition.
        read_at(record_addr)
    }
}

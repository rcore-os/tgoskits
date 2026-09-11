use core::ptr::NonNull;

use x86_64::{
    PrivilegeLevel, VirtAddr,
    instructions::tables::load_tss,
    registers::segmentation::{CS, DS, ES, SS, Segment, SegmentSelector},
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable},
        tss::TaskStateSegment,
    },
};

pub(super) const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Runtime-owned memory retained by this CPU's descriptor registers.
pub struct TrapStorage {
    /// A fresh, exclusively owned TSS for this CPU.
    pub tss: NonNull<TaskStateSegment>,
    /// A fresh, exclusively owned GDT for this CPU.
    pub gdt: NonNull<GlobalDescriptorTable>,
    /// Top of this CPU's mapped, writable double-fault stack.
    pub double_fault_stack_top: ax_memory_addr::VirtAddr,
}

/// Supplies descriptor storage once for each initialized CPU.
///
/// # Safety
///
/// Returned objects must be initialized, uniquely owned by the current CPU,
/// pinned, and mapped until CPU shutdown. The stack must be exclusive to that
/// CPU and large enough for its double-fault diagnostic path. Repeated calls
/// must fail before handing out an already installed object. In userspace
/// builds the provider must export `__CPU_LOCAL_TSS_OFFSET`, the absolute
/// offset of this same TSS from the kernel GS base, for pre-stack entry code.
#[trait_ffi::def_extern_trait(mod_path = "boot")]
pub unsafe trait TrapStorageProvider {
    /// Transfers this CPU's fresh storage to its descriptor initialization.
    fn take() -> TrapStorage;
}

/// Kernel code segment for 64-bit mode.
pub const KCODE64: SegmentSelector = SegmentSelector::new(1, PrivilegeLevel::Ring0);
/// Kernel data segment.
pub const KDATA: SegmentSelector = SegmentSelector::new(2, PrivilegeLevel::Ring0);
/// User data segment.
pub const UDATA: SegmentSelector = SegmentSelector::new(3, PrivilegeLevel::Ring3);
/// User code segment for 64-bit mode.
pub const UCODE64: SegmentSelector = SegmentSelector::new(4, PrivilegeLevel::Ring3);

fn install_exception_stacks(tss: &mut TaskStateSegment, double_fault_stack_top: VirtAddr) {
    tss.interrupt_stack_table[usize::from(DOUBLE_FAULT_IST_INDEX)] = double_fault_stack_top;
}

/// Initializes the per-CPU TSS and GDT structures and loads them into the
/// current CPU.
pub(super) fn init() {
    let mut storage = trap_storage_provider::take();
    // SAFETY: the provider contract transfers initialized, unique, permanent
    // storage. CPU bring-up owns this IRQ-disabled CPU until setup completes.
    let (gdt, tss): (
        &'static mut GlobalDescriptorTable,
        &'static mut TaskStateSegment,
    ) = unsafe { (storage.gdt.as_mut(), storage.tss.as_mut()) };
    install_exception_stacks(
        tss,
        VirtAddr::new(storage.double_fault_stack_top.as_usize() as u64),
    );
    assert_eq!(gdt.append(Descriptor::kernel_code_segment()), KCODE64);
    assert_eq!(gdt.append(Descriptor::kernel_data_segment()), KDATA);
    assert_eq!(gdt.append(Descriptor::user_data_segment()), UDATA);
    assert_eq!(gdt.append(Descriptor::user_code_segment()), UCODE64);
    let tss = gdt.append(Descriptor::tss_segment(&*tss));
    gdt.load();
    // SAFETY: the GDT above contains these selectors and its TSS descriptor
    // references the current CPU's permanent, initialized storage.
    unsafe {
        CS::set_reg(KCODE64);
        DS::set_reg(KDATA);
        ES::set_reg(KDATA);
        SS::set_reg(KDATA);
        load_tss(tss);
    }
}

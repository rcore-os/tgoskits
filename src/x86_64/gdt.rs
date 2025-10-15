use x86_64::{
    instructions::tables::load_tss,
    registers::segmentation::{Segment, SegmentSelector, CS},
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable},
        tss::TaskStateSegment,
    },
    PrivilegeLevel,
};

#[percpu::def_percpu]
#[unsafe(no_mangle)]
static TSS: TaskStateSegment = TaskStateSegment::new();

#[percpu::def_percpu]
static GDT: GlobalDescriptorTable = GlobalDescriptorTable::new();

/// Kernel code segment for 64-bit mode.
pub const KCODE64: SegmentSelector = SegmentSelector::new(1, PrivilegeLevel::Ring0);
/// Kernel data segment.
pub const KDATA: SegmentSelector = SegmentSelector::new(2, PrivilegeLevel::Ring0);
/// User data segment.
pub const UDATA: SegmentSelector = SegmentSelector::new(3, PrivilegeLevel::Ring3);
/// User code segment for 64-bit mode.
pub const UCODE64: SegmentSelector = SegmentSelector::new(4, PrivilegeLevel::Ring3);

/// Initializes the per-CPU TSS and GDT structures and loads them into the
/// current CPU.
pub(super) fn init() {
    let gdt = unsafe { GDT.current_ref_mut_raw() };
    assert_eq!(gdt.append(Descriptor::kernel_code_segment()), KCODE64);
    assert_eq!(gdt.append(Descriptor::kernel_data_segment()), KDATA);
    assert_eq!(gdt.append(Descriptor::user_data_segment()), UDATA);
    assert_eq!(gdt.append(Descriptor::user_code_segment()), UCODE64);
    let tss = gdt.append(Descriptor::tss_segment(unsafe { TSS.current_ref_raw() }));
    gdt.load();
    unsafe {
        CS::set_reg(KCODE64);
        load_tss(tss);
    }
}

use x86_64::{
    PrivilegeLevel,
    instructions::tables::load_tss,
    registers::segmentation::{CS, DS, ES, SS, Segment, SegmentSelector},
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable},
        tss::TaskStateSegment,
    },
};

#[ax_percpu::def_percpu]
#[unsafe(no_mangle)]
static TSS: TaskStateSegment = TaskStateSegment::new();

#[ax_percpu::def_percpu]
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
    // SAFETY: CPU initialization runs with migration and local interrupts
    // disabled before this CPU can re-enter GDT/TSS setup.
    let tss = unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            ax_percpu::with_exclusive_cpu(pin, |_exclusive| {
                let mut gdt = GDT.current_ptr(pin);
                let tss = TSS.current_ptr(pin);
                // SAFETY: dynamic CPU areas live until shutdown. This one-shot
                // CPU setup exclusively initializes the GDT, then the hardware
                // retains both pointers for this CPU's lifetime.
                let gdt: &'static mut GlobalDescriptorTable = gdt.as_mut();
                let tss: &'static TaskStateSegment = tss.as_ref();
                assert_eq!(gdt.append(Descriptor::kernel_code_segment()), KCODE64);
                assert_eq!(gdt.append(Descriptor::kernel_data_segment()), KDATA);
                assert_eq!(gdt.append(Descriptor::user_data_segment()), UDATA);
                assert_eq!(gdt.append(Descriptor::user_code_segment()), UCODE64);
                let tss = gdt.append(Descriptor::tss_segment(tss));
                gdt.load();
                tss
            })
        })
    }
    .expect("x86 GDT initialization requires an installed CPU area");
    unsafe {
        CS::set_reg(KCODE64);
        DS::set_reg(KDATA);
        ES::set_reg(KDATA);
        SS::set_reg(KDATA);
        load_tss(tss);
    }
}

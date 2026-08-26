use core::{cell::UnsafeCell, mem::size_of, ptr::NonNull};

use x86_64::{
    PrivilegeLevel, VirtAddr,
    instructions::tables::load_tss,
    registers::segmentation::{CS, DS, ES, SS, Segment, SegmentSelector},
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable},
        tss::TaskStateSegment,
    },
};

const DOUBLE_FAULT_STACK_SIZE: usize = 32 * 1024;
pub(super) const DOUBLE_FAULT_IST_INDEX: u16 = 0;

#[repr(C, align(4096))]
struct ExceptionStack {
    storage: UnsafeCell<[u8; DOUBLE_FAULT_STACK_SIZE]>,
}

impl ExceptionStack {
    const fn new() -> Self {
        Self {
            storage: UnsafeCell::new([0; DOUBLE_FAULT_STACK_SIZE]),
        }
    }

    fn top(stack: NonNull<Self>) -> VirtAddr {
        let base = stack.as_ptr().cast::<u8>() as usize;
        VirtAddr::new(
            base.checked_add(size_of::<Self>())
                .expect("x86 exception-stack address overflow") as u64,
        )
    }
}

#[ax_percpu::def_percpu]
#[unsafe(no_mangle)]
static TSS: TaskStateSegment = TaskStateSegment::new();

#[ax_percpu::def_percpu]
static GDT: GlobalDescriptorTable = GlobalDescriptorTable::new();

#[ax_percpu::def_percpu]
static DOUBLE_FAULT_STACK: ExceptionStack = ExceptionStack::new();

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
    // SAFETY: CPU initialization runs with migration and local interrupts
    // disabled before this CPU can re-enter GDT/TSS setup.
    let tss = unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            ax_percpu::with_exclusive_cpu(pin, |_exclusive| {
                let mut gdt = GDT.current_ptr(pin);
                let mut tss = TSS.current_ptr(pin);
                let double_fault_stack = DOUBLE_FAULT_STACK.current_ptr(pin);
                // SAFETY: dynamic CPU areas live until shutdown. This one-shot
                // CPU setup exclusively initializes the GDT, TSS, and
                // exception-stack pointer, then the hardware retains all three
                // objects for this CPU's lifetime.
                let gdt: &'static mut GlobalDescriptorTable = gdt.as_mut();
                let tss: &'static mut TaskStateSegment = tss.as_mut();
                install_exception_stacks(tss, ExceptionStack::top(double_fault_stack));
                assert_eq!(gdt.append(Descriptor::kernel_code_segment()), KCODE64);
                assert_eq!(gdt.append(Descriptor::kernel_data_segment()), KDATA);
                assert_eq!(gdt.append(Descriptor::user_data_segment()), UDATA);
                assert_eq!(gdt.append(Descriptor::user_code_segment()), UCODE64);
                let tss = gdt.append(Descriptor::tss_segment(&*tss));
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

#[cfg(test)]
mod tests {
    use x86_64::{VirtAddr, structures::tss::TaskStateSegment};

    use super::{DOUBLE_FAULT_IST_INDEX, install_exception_stacks};

    #[test]
    fn tss_owns_the_double_fault_stack_top() {
        let mut tss = TaskStateSegment::new();
        let top = VirtAddr::new(0xffff_8000_0001_0000);

        install_exception_stacks(&mut tss, top);

        let installed = tss.interrupt_stack_table[usize::from(DOUBLE_FAULT_IST_INDEX)];
        assert_eq!(installed, top);
    }
}

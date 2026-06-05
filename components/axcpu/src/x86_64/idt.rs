use ax_lazyinit::LazyInit;
use x86_64::{
    addr::VirtAddr,
    structures::idt::{Entry, InterruptDescriptorTable},
};

const NUM_INT: usize = 256;

static IDT: LazyInit<InterruptDescriptorTable> = LazyInit::new();

/// Initializes the global IDT and loads it into the current CPU.
pub(super) fn init() {
    IDT.call_once(|| {
        unsafe extern "C" {
            #[link_name = "trap_handler_table"]
            static ENTRIES: [i32; NUM_INT];
        }
        let mut table = InterruptDescriptorTable::new();
        let entries = unsafe {
            core::mem::transmute::<&mut InterruptDescriptorTable, &mut [Entry<()>; NUM_INT]>(
                &mut table,
            )
        };
        let base = unsafe { ENTRIES.as_ptr() } as isize;
        for (i, entry) in entries.iter_mut().enumerate() {
            let offset = unsafe { *ENTRIES.as_ptr().add(i) } as isize;
            let handler = VirtAddr::new((base + offset) as u64);
            let opt = unsafe { entry.set_handler_addr(handler) };
            if i == 0x3 || i == 0x80 {
                // enable user space breakpoints and legacy int 0x80 syscall
                opt.set_privilege_level(x86_64::PrivilegeLevel::Ring3);
            }
        }

        table
    });
    IDT.load();
}

use lazyinit::LazyInit;
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
            static ENTRIES: [VirtAddr; NUM_INT];
        }
        let mut table = InterruptDescriptorTable::new();
        let entries = unsafe {
            core::mem::transmute::<&mut InterruptDescriptorTable, &mut [Entry<()>; NUM_INT]>(
                &mut table,
            )
        };
        for i in 0..NUM_INT {
            let opt = unsafe { entries[i].set_handler_addr(ENTRIES[i]) };
            if i == 0x3 || i == 0x80 {
                // enable user space breakpoints and legacy int 0x80 syscall
                opt.set_privilege_level(x86_64::PrivilegeLevel::Ring3);
            }
        }

        table
    });
    IDT.load();
}

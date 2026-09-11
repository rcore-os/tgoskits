use core::{cell::UnsafeCell, mem::size_of};

use ax_cpu::boot::{GlobalDescriptorTable, TaskStateSegment, TrapStorage};

const DOUBLE_FAULT_STACK_SIZE: usize = 32 * 1024;

#[repr(C, align(4096))]
struct ExceptionStack(UnsafeCell<[u8; DOUBLE_FAULT_STACK_SIZE]>);

#[ax_percpu::def_percpu]
#[unsafe(no_mangle)]
static TSS: TaskStateSegment = TaskStateSegment::new();

#[ax_percpu::def_percpu]
static GDT: GlobalDescriptorTable = GlobalDescriptorTable::new();

#[ax_percpu::def_percpu]
static DOUBLE_FAULT_STACK: ExceptionStack =
    ExceptionStack(UnsafeCell::new([0; DOUBLE_FAULT_STACK_SIZE]));

#[ax_percpu::def_percpu]
static TAKEN: bool = false;

struct RuntimeTrapStorage;

// SAFETY: dynamic CPU areas remain pinned and mapped until shutdown. Each CPU
// owns separate zero-initialized descriptor and stack storage. TAKEN rejects
// duplicate transfers before pointers escape; the IRQ-disabled CPU pin and
// exclusive scope exclude local reentry throughout the transfer. TSS's exported
// per-CPU offset is the same symbol consumed by CPU syscall assembly.
ax_cpu::boot::trap_storage_provider::impl_trait! {
    unsafe impl TrapStorageProvider for RuntimeTrapStorage {
        fn take() -> TrapStorage {
            assert!(!ax_cpu::interrupt::irqs_enabled(), "descriptor setup requires IRQ exclusion");
            // SAFETY: CPU bring-up has installed the CPU area and excludes
            // migration and interrupts until descriptor installation completes.
            unsafe {
                ax_percpu::with_cpu_pin(|pin| {
                    ax_percpu::with_exclusive_cpu(pin, |_exclusive| {
                        let mut taken = TAKEN.current_ptr(pin);
                        assert!(!*taken.as_ptr(), "CPU descriptor storage already installed");
                        let stack = DOUBLE_FAULT_STACK.current_ptr(pin);
                        let top = (stack.as_ptr() as usize)
                            .checked_add(size_of::<ExceptionStack>())
                            .expect("x86 exception-stack address overflow");
                        let storage = TrapStorage {
                            tss: TSS.current_ptr(pin),
                            gdt: GDT.current_ptr(pin),
                            double_fault_stack_top: top.into(),
                        };
                        *taken.as_mut() = true;
                        storage
                    })
                })
            }.expect("x86 descriptor setup requires an installed CPU area")
        }
    }
}

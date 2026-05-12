use core::{
    arch::global_asm,
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use x86::msr::{IA32_APIC_BASE, rdmsr};

use crate::{mem::phys_to_virt, power::CpuOnError, smp::PerCpuMeta};

pub const AP_TRAMPOLINE_PADDR: usize = 0x8000;
const AP_TRAMPOLINE_VECTOR: u8 = (AP_TRAMPOLINE_PADDR >> 12) as u8;
const AP_TRAMPOLINE_SIZE: usize = 0x1000;
const AP_START_TIMEOUT_US: u64 = 500_000;

const LAPIC_REG_ESR: u32 = 0x280;
const LAPIC_REG_ICR_LOW: u32 = 0x300;
const LAPIC_REG_ICR_HIGH: u32 = 0x310;

// INIT IPI (level-triggered): assert then deassert.
const ICR_INIT_ASSERT: u32 = 0x0000_c500;
const ICR_INIT_DEASSERT: u32 = 0x0000_8500;
// STARTUP IPI (edge-triggered + assert level)
const ICR_STARTUP_BASE: u32 = 0x0000_4600;

static START_LOCK: AtomicBool = AtomicBool::new(false);
static AP_BOOTED_ID: AtomicUsize = AtomicUsize::new(usize::MAX);

global_asm!(
    r#"
    .section .text.ap_trampoline, "ax"
    .balign 16
    .global __x86_ap_trampoline_start
    .global __x86_ap_trampoline_end
    .global __x86_ap_long_mode
    .global __x86_ap_gdt
    .global __x86_ap_gdt_ptr_base
    .global __x86_ap_ljmp_ptr_offset
    .global __x86_ap_trampoline_cr3
    .global __x86_ap_trampoline_stack
    .global __x86_ap_trampoline_arg
    .global __x86_ap_trampoline_entry
__x86_ap_trampoline_start:
    .code16
    cli
    cld
    movw %cs, %ax
    movw %ax, %ds
    movw %ax, %ss
    movw $0x7000, %sp

    lgdt (__x86_ap_gdt_ptr - __x86_ap_trampoline_start)

    movl %cr4, %eax
    orl $0x20, %eax
    movl %eax, %cr4

    movl (__x86_ap_trampoline_cr3 - __x86_ap_trampoline_start), %eax
    movl %eax, %cr3

    movl $0xC0000080, %ecx
    rdmsr
    orl $0x00000100, %eax
    wrmsr

    movl %cr0, %eax
    orl $0x80000001, %eax
    movl %eax, %cr0

    ljmpl *(__x86_ap_ljmp_ptr - __x86_ap_trampoline_start)

    .code64
__x86_ap_long_mode:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw %ax, %fs
    movw %ax, %gs

    movq __x86_ap_trampoline_stack(%rip), %rsp
    movq __x86_ap_trampoline_arg(%rip), %rdi
    movq __x86_ap_trampoline_entry(%rip), %rax
    jmp *%rax

    .balign 8
__x86_ap_ljmp_ptr:
__x86_ap_ljmp_ptr_offset:
    .long 0
    .word 0x08

    .balign 8
__x86_ap_gdt_ptr:
    .word 0x17
__x86_ap_gdt_ptr_base:
    .long 0

    .balign 8
__x86_ap_gdt:
    .quad 0x0000000000000000
    .quad 0x00af9a000000ffff
    .quad 0x00af92000000ffff

    .balign 8
__x86_ap_trampoline_cr3:
    .quad 0
__x86_ap_trampoline_stack:
    .quad 0
__x86_ap_trampoline_arg:
    .quad 0
__x86_ap_trampoline_entry:
    .quad 0
__x86_ap_trampoline_end:
"#,
    options(att_syntax)
);

unsafe extern "C" {
    static __x86_ap_trampoline_start: u8;
    static __x86_ap_trampoline_end: u8;
    static __x86_ap_long_mode: u8;
    static __x86_ap_gdt: u8;
    static __x86_ap_gdt_ptr_base: u8;
    static __x86_ap_ljmp_ptr_offset: u8;
    static __x86_ap_trampoline_cr3: u8;
    static __x86_ap_trampoline_stack: u8;
    static __x86_ap_trampoline_arg: u8;
    static __x86_ap_trampoline_entry: u8;
}

pub(crate) fn notify_ap_started(apic_id: usize) {
    AP_BOOTED_ID.store(apic_id, Ordering::Release);
}

pub(crate) fn cpu_on(apic_id: usize, entry: usize, arg: usize) -> Result<(), CpuOnError> {
    if apic_id == crate::smp::cpu_hart_id() {
        return Err(CpuOnError::AlreadyOn);
    }
    let apic_id = u8::try_from(apic_id).map_err(|_| CpuOnError::InvalidParameters)?;
    let meta = unsafe { &*(phys_to_virt(arg) as *const PerCpuMeta) };
    if meta.primary_table_paddr > u32::MAX as usize {
        return Err(CpuOnError::Other(anyhow::anyhow!(
            "x86 AP startup requires <4G CR3, got {:#x}",
            meta.primary_table_paddr
        )));
    }

    let _guard = StartupGuard::lock();
    AP_BOOTED_ID.store(usize::MAX, Ordering::Release);
    let entry_virt = crate::mem::__kimage_va(entry) as usize;
    prepare_trampoline(
        meta.primary_table_paddr as u64,
        meta.stack_top_virt as u64,
        arg as u64,
        entry_virt as u64,
    );

    unsafe {
        send_ipi(apic_id, ICR_INIT_ASSERT);
    }
    delay_us(10_000);
    unsafe {
        send_ipi(apic_id, ICR_INIT_DEASSERT);
    }
    delay_us(200);
    unsafe {
        send_ipi(apic_id, ICR_STARTUP_BASE | AP_TRAMPOLINE_VECTOR as u32);
    }
    delay_us(200);
    unsafe {
        send_ipi(apic_id, ICR_STARTUP_BASE | AP_TRAMPOLINE_VECTOR as u32);
    }

    let start = super::trap::ticks_now();
    let timeout_ticks = us_to_tsc_ticks(AP_START_TIMEOUT_US);
    while super::trap::ticks_now().wrapping_sub(start) < timeout_ticks {
        if AP_BOOTED_ID.load(Ordering::Acquire) == apic_id as usize {
            return Ok(());
        }
        spin_loop();
    }

    Err(CpuOnError::Other(anyhow::anyhow!(
        "timeout waiting APIC ID {:#x} online",
        apic_id
    )))
}

fn us_to_tsc_ticks(us: u64) -> u64 {
    let freq = super::trap::tsc_freq() as u128;
    let ticks = (freq * us as u128) / 1_000_000;
    ticks.max(1) as u64
}

fn delay_us(us: u64) {
    let start = super::trap::ticks_now();
    let target = us_to_tsc_ticks(us);
    while super::trap::ticks_now().wrapping_sub(start) < target {
        spin_loop();
    }
}

fn prepare_trampoline(cr3: u64, stack: u64, arg: u64, entry: u64) {
    let src_start = core::ptr::addr_of!(__x86_ap_trampoline_start);
    let src_end = core::ptr::addr_of!(__x86_ap_trampoline_end);
    let len = src_end as usize - src_start as usize;
    assert!(len <= AP_TRAMPOLINE_SIZE);

    let dst = AP_TRAMPOLINE_PADDR as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(src_start, dst, len);
    }

    let gdt_base = AP_TRAMPOLINE_PADDR + sym_offset(core::ptr::addr_of!(__x86_ap_gdt));
    let long_mode = AP_TRAMPOLINE_PADDR + sym_offset(core::ptr::addr_of!(__x86_ap_long_mode));

    unsafe {
        write_u32(
            dst,
            sym_offset(core::ptr::addr_of!(__x86_ap_gdt_ptr_base)),
            gdt_base as u32,
        );
        write_u32(
            dst,
            sym_offset(core::ptr::addr_of!(__x86_ap_ljmp_ptr_offset)),
            long_mode as u32,
        );
        write_u64(
            dst,
            sym_offset(core::ptr::addr_of!(__x86_ap_trampoline_cr3)),
            cr3,
        );
        write_u64(
            dst,
            sym_offset(core::ptr::addr_of!(__x86_ap_trampoline_stack)),
            stack,
        );
        write_u64(
            dst,
            sym_offset(core::ptr::addr_of!(__x86_ap_trampoline_arg)),
            arg,
        );
        write_u64(
            dst,
            sym_offset(core::ptr::addr_of!(__x86_ap_trampoline_entry)),
            entry,
        );
    }

    core::sync::atomic::fence(Ordering::SeqCst);
}

fn sym_offset(sym: *const u8) -> usize {
    let start = core::ptr::addr_of!(__x86_ap_trampoline_start) as usize;
    sym as usize - start
}

unsafe fn write_u32(base: *mut u8, offset: usize, value: u32) {
    unsafe {
        core::ptr::write_unaligned(base.add(offset).cast::<u32>(), value);
    }
}

unsafe fn write_u64(base: *mut u8, offset: usize, value: u64) {
    unsafe {
        core::ptr::write_unaligned(base.add(offset).cast::<u64>(), value);
    }
}

fn lapic_base() -> usize {
    (unsafe { rdmsr(IA32_APIC_BASE) } as usize) & !(crate::mem::page_size() - 1)
}

unsafe fn lapic_write(offset: u32, value: u32) {
    let ptr = (lapic_base() + offset as usize) as *mut u32;
    unsafe {
        ptr.write_volatile(value);
    }
}

unsafe fn lapic_read(offset: u32) -> u32 {
    let ptr = (lapic_base() + offset as usize) as *const u32;
    unsafe { ptr.read_volatile() }
}

unsafe fn send_ipi(apic_id: u8, icr_low: u32) {
    unsafe {
        lapic_write(LAPIC_REG_ESR, 0);
        lapic_write(LAPIC_REG_ESR, 0);
        lapic_write(LAPIC_REG_ICR_HIGH, (apic_id as u32) << 24);
        lapic_write(LAPIC_REG_ICR_LOW, icr_low);
        while lapic_read(LAPIC_REG_ICR_LOW) & (1 << 12) != 0 {
            spin_loop();
        }
    }
}

struct StartupGuard;

impl StartupGuard {
    fn lock() -> Self {
        while START_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        Self
    }
}

impl Drop for StartupGuard {
    fn drop(&mut self) {
        START_LOCK.store(false, Ordering::Release);
    }
}

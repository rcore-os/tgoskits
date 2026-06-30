use core::{
    arch::global_asm,
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use x86::msr::{
    IA32_APIC_BASE, IA32_X2APIC_APICID, IA32_X2APIC_ESR, IA32_X2APIC_ICR, rdmsr, wrmsr,
};

use crate::{mem::phys_to_virt, power::CpuOnError, smp::PerCpuMeta};

pub const AP_TRAMPOLINE_PADDR: usize = 0x8000;
const AP_TRAMPOLINE_VECTOR: u8 = (AP_TRAMPOLINE_PADDR >> 12) as u8;
const AP_TRAMPOLINE_SIZE: usize = 0x1000;
const AP_START_TIMEOUT_US: u64 = 500_000;

const LAPIC_REG_ESR: u32 = 0x280;
const LAPIC_REG_ICR_LOW: u32 = 0x300;
const LAPIC_REG_ICR_HIGH: u32 = 0x310;
const ICR_DELIVERY_PENDING: u32 = 1 << 12;
const IPI_DELIVERY_WAIT_SPINS: usize = 1_000_000;
const IA32_APIC_BASE_X2APIC_ENABLE: u64 = 1 << 10;

// INIT IPI (level-triggered): assert then deassert.
const ICR_INIT_ASSERT: u32 = 0x0000_c500;
const ICR_INIT_DEASSERT: u32 = 0x0000_8500;
// STARTUP IPI (edge-triggered + assert level)
const ICR_STARTUP_BASE: u32 = 0x0000_4600;

static START_LOCK: AtomicBool = AtomicBool::new(false);
static AP_BOOTED_ID: AtomicUsize = AtomicUsize::new(usize::MAX);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApicMode {
    XApic,
    X2Apic,
}

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

    # Enable PAE plus OS-managed FXSAVE/SSE state before entering long mode.
    movl %cr4, %eax
    orl $0x620, %eax
    movl %eax, %cr4

    movl (__x86_ap_trampoline_cr3 - __x86_ap_trampoline_start), %eax
    movl %eax, %cr3

    movl $0xC0000080, %ecx
    rdmsr
    orl $0x00000100, %eax
    wrmsr

    # Clear EM/TS and enable protected mode, paging, MP, and native FP errors.
    movl %cr0, %eax
    andl $0xfffffff3, %eax
    orl $0x80000023, %eax
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

fn current_apic_id() -> usize {
    match current_apic_mode() {
        ApicMode::X2Apic => unsafe { rdmsr(IA32_X2APIC_APICID) as usize },
        ApicMode::XApic => x86::cpuid::CpuId::new()
            .get_feature_info()
            .map(|info| info.initial_local_apic_id() as usize)
            .unwrap_or(0),
    }
}

pub(crate) fn cpu_on(apic_id: usize, entry: usize, arg: usize) -> Result<(), CpuOnError> {
    if apic_id == current_apic_id() {
        return Err(CpuOnError::AlreadyOn);
    }
    let apic_id = u32::try_from(apic_id).map_err(|_| CpuOnError::InvalidParameters)?;
    let meta = unsafe { &*(phys_to_virt(arg) as *const PerCpuMeta) };
    if meta.boot_table_paddr > u32::MAX as usize {
        return Err(CpuOnError::Other(anyhow::anyhow!(
            "x86 AP startup requires <4G CR3, got {:#x}",
            meta.boot_table_paddr
        )));
    }

    let _guard = StartupGuard::lock();
    AP_BOOTED_ID.store(usize::MAX, Ordering::Release);
    let entry_virt = crate::mem::__kimage_va(entry) as usize;
    prepare_trampoline(
        meta.boot_table_paddr as u64,
        meta.stack_top_virt as u64,
        arg as u64,
        entry_virt as u64,
    );

    send_ipi(apic_id, ICR_INIT_ASSERT)?;
    delay_us(10_000);
    send_ipi(apic_id, ICR_INIT_DEASSERT)?;
    delay_us(200);
    send_ipi(apic_id, ICR_STARTUP_BASE | AP_TRAMPOLINE_VECTOR as u32)?;
    delay_us(200);
    send_ipi(apic_id, ICR_STARTUP_BASE | AP_TRAMPOLINE_VECTOR as u32)?;

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

    let dst = phys_to_virt(AP_TRAMPOLINE_PADDR);
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

fn lapic_base() -> *mut u8 {
    let base = (unsafe { rdmsr(IA32_APIC_BASE) } as usize) & !(crate::mem::page_size() - 1);
    phys_to_virt(base)
}

unsafe fn lapic_write(offset: u32, value: u32) {
    let ptr = unsafe { lapic_base().add(offset as usize) }.cast::<u32>();
    unsafe {
        ptr.write_volatile(value);
    }
}

unsafe fn lapic_read(offset: u32) -> u32 {
    let ptr = unsafe { lapic_base().add(offset as usize) }.cast::<u32>();
    unsafe { ptr.read_volatile() }
}

fn current_apic_mode() -> ApicMode {
    let base = unsafe { rdmsr(IA32_APIC_BASE) };
    if base & IA32_APIC_BASE_X2APIC_ENABLE != 0 {
        ApicMode::X2Apic
    } else {
        ApicMode::XApic
    }
}

fn xapic_destination(apic_id: u32) -> Result<u32, CpuOnError> {
    let dest = u8::try_from(apic_id).map_err(|_| CpuOnError::InvalidParameters)?;
    Ok(u32::from(dest) << 24)
}

fn x2apic_icr(apic_id: u32, icr_low: u32) -> u64 {
    (u64::from(apic_id) << 32) | u64::from(icr_low)
}

fn send_ipi(apic_id: u32, icr_low: u32) -> Result<(), CpuOnError> {
    match current_apic_mode() {
        ApicMode::X2Apic => send_x2apic_ipi(x2apic_icr(apic_id, icr_low)),
        ApicMode::XApic => send_xapic_ipi(xapic_destination(apic_id)?, icr_low),
    }
}

fn send_xapic_ipi(destination: u32, icr_low: u32) -> Result<(), CpuOnError> {
    unsafe {
        lapic_write(LAPIC_REG_ESR, 0);
        lapic_write(LAPIC_REG_ESR, 0);
        lapic_write(LAPIC_REG_ICR_HIGH, destination);
        lapic_write(LAPIC_REG_ICR_LOW, icr_low);
    }
    wait_xapic_delivery()
}

fn send_x2apic_ipi(icr: u64) -> Result<(), CpuOnError> {
    unsafe {
        wrmsr(IA32_X2APIC_ESR, 0);
        wrmsr(IA32_X2APIC_ESR, 0);
        wrmsr(IA32_X2APIC_ICR, icr);
    }
    wait_x2apic_delivery()
}

fn wait_xapic_delivery() -> Result<(), CpuOnError> {
    for _ in 0..IPI_DELIVERY_WAIT_SPINS {
        if unsafe { lapic_read(LAPIC_REG_ICR_LOW) } & ICR_DELIVERY_PENDING == 0 {
            return Ok(());
        }
        spin_loop();
    }
    Err(CpuOnError::Other(anyhow::anyhow!(
        "timeout waiting xAPIC IPI delivery"
    )))
}

fn wait_x2apic_delivery() -> Result<(), CpuOnError> {
    for _ in 0..IPI_DELIVERY_WAIT_SPINS {
        if unsafe { rdmsr(IA32_X2APIC_ICR) } & u64::from(ICR_DELIVERY_PENDING) == 0 {
            return Ok(());
        }
        spin_loop();
    }
    Err(CpuOnError::Other(anyhow::anyhow!(
        "timeout waiting x2APIC IPI delivery"
    )))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xapic_destination_rejects_high_apic_ids_without_truncation() {
        assert!(matches!(xapic_destination(0xff), Ok(0xff00_0000)));
        assert!(matches!(
            xapic_destination(0x100),
            Err(CpuOnError::InvalidParameters)
        ));
    }

    #[test]
    fn x2apic_icr_encodes_full_destination_id() {
        let icr = x2apic_icr(0x1234_5678, ICR_STARTUP_BASE | AP_TRAMPOLINE_VECTOR as u32);

        assert_eq!(icr >> 32, 0x1234_5678);
        assert_eq!(icr as u32, ICR_STARTUP_BASE | AP_TRAMPOLINE_VECTOR as u32);
    }
}

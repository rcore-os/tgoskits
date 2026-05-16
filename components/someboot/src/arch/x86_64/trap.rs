use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

use page_table_generic::PhysAddr;
use x86::{
    bits64::{rflags, segmentation::Descriptor64},
    controlregs,
    cpuid::CpuId,
    dtables::{self, DescriptorTablePointer},
    irq::PageFaultError,
    msr::{self, rdmsr, wrmsr},
    segmentation::{BuildDescriptor, DescriptorBuilder, GateDescriptorBuilder, cs},
};

use super::irq::{LAPIC_SPURIOUS_VECTOR, LAPIC_TIMER_VECTOR};
use crate::{irq, mem::page_size};

const IA32_EFER: u32 = 0xc000_0080;
const IA32_EFER_NXE: u64 = 1 << 11;

const LAPIC_REG_EOI: u32 = 0x0b0;
const LAPIC_REG_SVR: u32 = 0x0f0;
const LAPIC_REG_LVT_TIMER: u32 = 0x320;
const LAPIC_REG_TIMER_INIT_COUNT: u32 = 0x380;
const LAPIC_REG_TIMER_CUR_COUNT: u32 = 0x390;
const LAPIC_REG_TIMER_DIV: u32 = 0x3e0;
const LAPIC_LVT_MASKED: u32 = 1 << 16;
const LAPIC_LVT_TIMER_TSC_DEADLINE: u32 = 1 << 18;
const LAPIC_SVR_ENABLE: u32 = 1 << 8;
const LAPIC_BASE_MASK: u64 = 0xffff_f000;
const IA32_APIC_BASE_ENABLE: u64 = 1 << 11;
const LAPIC_TIMER_DIVIDE_BY_16: u32 = 0b0011;

static TSC_FREQ_HZ: AtomicU64 = AtomicU64::new(0);
static APIC_COUNTS_PER_TSC_Q32: AtomicU64 = AtomicU64::new(0);
static HAS_TSC_DEADLINE: AtomicBool = AtomicBool::new(false);
static LAPIC_READY: AtomicBool = AtomicBool::new(false);
static TSC_INFO_STATE: AtomicU8 = AtomicU8::new(0);
static IDT_STATE: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct InterruptStackFrame {
    instruction_pointer: u64,
    code_segment: u64,
    cpu_flags: u64,
    stack_pointer: u64,
    stack_segment: u64,
}

#[repr(C, align(16))]
struct Idt([Descriptor64; 256]);

static mut IDT: Idt = Idt([Descriptor64::NULL; 256]);

pub fn setup() {
    init_idt_once();
    load_idt();
}

pub fn trap_addr() -> usize {
    let mut ptr: DescriptorTablePointer<Descriptor64> = Default::default();
    unsafe {
        dtables::sidt(&mut ptr);
    }
    ptr.base as usize
}

pub fn init_local() {
    mask_legacy_pic();
    enable_nxe();
    init_tsc_freq();
    init_lapic();
}

pub fn timer_enable() {
    ensure_lapic_ready();
    set_lvt_masked(false);
    write_lapic_reg(LAPIC_REG_LVT_TIMER, timer_lvt_value(false));
}

pub fn timer_irq_enable() {
    ensure_lapic_ready();
    set_lvt_masked(false);
}

pub fn timer_irq_disable() {
    ensure_lapic_ready();
    set_lvt_masked(true);
}

pub fn timer_irq_is_enabled() -> bool {
    ensure_lapic_ready();
    (read_lapic_reg(LAPIC_REG_LVT_TIMER) & LAPIC_LVT_MASKED) == 0
}

pub fn timer_set_deadline_in_ticks(ticks: usize) {
    ensure_lapic_ready();
    if has_tsc_deadline() {
        let now = ticks_now();
        let deadline = now.saturating_add(ticks.max(1) as u64);
        unsafe {
            wrmsr(msr::IA32_TSC_DEADLINE, deadline);
        }
    } else {
        let counts = ticks_to_apic_counts(ticks.max(1) as u64);
        write_lapic_reg(LAPIC_REG_TIMER_INIT_COUNT, counts);
    }
}

pub fn timer_ack() {
    if LAPIC_READY.load(Ordering::Relaxed) {
        write_lapic_reg(LAPIC_REG_EOI, 0);
    }
}

pub fn tsc_freq() -> usize {
    let freq = TSC_FREQ_HZ.load(Ordering::Acquire);
    if freq == 0 {
        panic!("x86_64 TSC frequency is not initialized");
    }
    freq as usize
}

pub fn ticks_now() -> u64 {
    unsafe { x86::time::rdtsc() }
}

unsafe fn set_gate(
    index: usize,
    selector: x86::segmentation::SegmentSelector,
    offset: u64,
    trap: bool,
) {
    let builder = if trap {
        DescriptorBuilder::trap_gate_descriptor(selector, offset)
    } else {
        DescriptorBuilder::interrupt_descriptor(selector, offset)
    }
    .present();
    unsafe {
        IDT.0[index] = builder.finish();
    }
}

fn init_tsc_freq() {
    if TSC_INFO_STATE.load(Ordering::Acquire) == 2 {
        return;
    }
    if TSC_INFO_STATE
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        while TSC_INFO_STATE.load(Ordering::Acquire) != 2 {
            spin_loop();
        }
        return;
    }

    let cpuid = CpuId::new();
    let freq_hz = cpuid
        .get_tsc_info()
        .and_then(|info| {
            if let Some(freq) = info.tsc_frequency() {
                return Some(freq);
            }

            if info.numerator() != 0 && info.denominator() != 0 {
                let base_hz = cpuid
                    .get_processor_frequency_info()
                    .map(|pinfo| pinfo.processor_base_frequency() as u64 * 1_000_000)
                    .or_else(|| {
                        cpuid
                            .get_hypervisor_info()
                            .and_then(|hv| hv.tsc_frequency().map(|khz| khz as u64 * 1_000))
                    })?;
                let crystal_hz = base_hz * info.denominator() as u64 / info.numerator() as u64;
                return Some(crystal_hz * info.numerator() as u64 / info.denominator() as u64);
            }

            None
        })
        .or_else(|| {
            cpuid
                .get_processor_frequency_info()
                .map(|pinfo| pinfo.processor_base_frequency() as u64 * 1_000_000)
        })
        .or_else(|| {
            cpuid
                .get_hypervisor_info()
                .and_then(|hv| hv.tsc_frequency().map(|khz| khz as u64 * 1_000))
        })
        .filter(|freq| *freq != 0)
        .unwrap_or_else(|| {
            let fallback = 1_000_000_000u64;
            warn!("x86_64 TSC frequency unavailable, fallback to {fallback} Hz");
            fallback
        });

    let has_deadline = cpuid
        .get_feature_info()
        .is_some_and(|info| info.has_tsc_deadline());

    HAS_TSC_DEADLINE.store(has_deadline, Ordering::Release);
    if !has_deadline {
        warn!("x86_64 CPU has no TSC deadline timer, fallback to LAPIC one-shot");
    }

    TSC_FREQ_HZ.store(freq_hz, Ordering::Release);
    TSC_INFO_STATE.store(2, Ordering::Release);
}

fn init_idt_once() {
    if IDT_STATE.load(Ordering::Acquire) == 2 {
        return;
    }
    if IDT_STATE
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        while IDT_STATE.load(Ordering::Acquire) != 2 {
            spin_loop();
        }
        return;
    }

    unsafe {
        let selector = cs();
        set_gate(
            3,
            selector,
            breakpoint_handler as *const () as usize as u64,
            true,
        );
        set_gate(
            13,
            selector,
            general_protection_handler as *const () as usize as u64,
            false,
        );
        set_gate(
            14,
            selector,
            page_fault_handler as *const () as usize as u64,
            false,
        );
        set_gate(
            LAPIC_TIMER_VECTOR as usize,
            selector,
            lapic_timer_handler as *const () as usize as u64,
            false,
        );
        set_gate(
            LAPIC_SPURIOUS_VECTOR as usize,
            selector,
            spurious_handler as *const () as usize as u64,
            false,
        );
    }

    IDT_STATE.store(2, Ordering::Release);
}

fn load_idt() {
    unsafe {
        let ptr = DescriptorTablePointer {
            base: core::ptr::addr_of!(IDT.0).cast::<Descriptor64>(),
            limit: (core::mem::size_of::<Idt>() - 1) as u16,
        };
        dtables::lidt(&ptr);
    }
}

fn init_lapic() {
    let mut base = unsafe { rdmsr(msr::IA32_APIC_BASE) };
    base |= IA32_APIC_BASE_ENABLE;
    unsafe {
        wrmsr(msr::IA32_APIC_BASE, base);
    }

    write_lapic_reg(
        LAPIC_REG_SVR,
        LAPIC_SVR_ENABLE | LAPIC_SPURIOUS_VECTOR as u32,
    );
    write_lapic_reg(LAPIC_REG_TIMER_DIV, LAPIC_TIMER_DIVIDE_BY_16);
    write_lapic_reg(LAPIC_REG_LVT_TIMER, timer_lvt_value(true));
    if !has_tsc_deadline() {
        calibrate_apic_timer_ratio();
    }
    timer_ack();

    LAPIC_READY.store(true, Ordering::Release);
}

fn ensure_lapic_ready() {
    assert!(
        LAPIC_READY.load(Ordering::Acquire),
        "local APIC is not initialized"
    );
}

fn set_lvt_masked(masked: bool) {
    let mut val = read_lapic_reg(LAPIC_REG_LVT_TIMER);
    if masked {
        val |= LAPIC_LVT_MASKED;
    } else {
        val &= !LAPIC_LVT_MASKED;
    }
    write_lapic_reg(LAPIC_REG_LVT_TIMER, val);
}

#[inline]
fn has_tsc_deadline() -> bool {
    HAS_TSC_DEADLINE.load(Ordering::Acquire)
}

#[inline]
fn timer_lvt_value(masked: bool) -> u32 {
    let mut val = LAPIC_TIMER_VECTOR as u32;
    if masked {
        val |= LAPIC_LVT_MASKED;
    }
    if has_tsc_deadline() {
        val |= LAPIC_LVT_TIMER_TSC_DEADLINE;
    }
    val
}

fn calibrate_apic_timer_ratio() {
    let wait_tsc = (tsc_freq() as u64 / 100).max(1); // target ~=10ms in TSC domain

    write_lapic_reg(LAPIC_REG_TIMER_INIT_COUNT, u32::MAX);
    let start_tsc = ticks_now();
    loop {
        if ticks_now().wrapping_sub(start_tsc) >= wait_tsc {
            break;
        }
        core::hint::spin_loop();
    }
    let end_tsc = ticks_now();
    let current = read_lapic_reg(LAPIC_REG_TIMER_CUR_COUNT);
    write_lapic_reg(LAPIC_REG_TIMER_INIT_COUNT, 0);

    let elapsed_tsc = end_tsc.wrapping_sub(start_tsc);
    let elapsed_apic = (u32::MAX - current) as u64;
    let q32 = if elapsed_tsc == 0 || elapsed_apic == 0 {
        1u64 << 32
    } else {
        (((elapsed_apic as u128) << 32) / elapsed_tsc as u128) as u64
    };
    APIC_COUNTS_PER_TSC_Q32.store(q32, Ordering::Release);
}

fn ticks_to_apic_counts(ticks: u64) -> u32 {
    let q32 = APIC_COUNTS_PER_TSC_Q32.load(Ordering::Acquire);
    let q32 = if q32 == 0 { 1u64 << 32 } else { q32 };
    let counts = ((ticks as u128 * q32 as u128) >> 32).max(1);
    counts.min(u32::MAX as u128) as u32
}

fn read_lapic_reg(offset: u32) -> u32 {
    let ptr = lapic_ptr(offset);
    unsafe { ptr.read_volatile() }
}

fn write_lapic_reg(offset: u32, value: u32) {
    let ptr = lapic_ptr(offset);
    unsafe {
        ptr.write_volatile(value);
    }
}

fn lapic_ptr(offset: u32) -> *mut u32 {
    let base = unsafe { rdmsr(msr::IA32_APIC_BASE) & LAPIC_BASE_MASK } as usize;
    (base + offset as usize) as *mut u32
}

fn enable_nxe() {
    let efer = unsafe { rdmsr(IA32_EFER) } | IA32_EFER_NXE;
    unsafe {
        wrmsr(IA32_EFER, efer);
    }
}

fn mask_legacy_pic() {
    unsafe {
        x86::io::outb(0x21, 0xff);
        x86::io::outb(0xa1, 0xff);
    }
}

extern "x86-interrupt" fn breakpoint_handler(frame: InterruptStackFrame) {
    println!("x86_64 breakpoint: {frame:#x?}");
}

extern "x86-interrupt" fn general_protection_handler(frame: InterruptStackFrame, error_code: u64) {
    panic!("x86_64 general protection fault: error={error_code:#x}, frame={frame:#x?}");
}

extern "x86-interrupt" fn page_fault_handler(frame: InterruptStackFrame, error_code: u64) {
    let addr = unsafe { controlregs::cr2() };
    let flags = PageFaultError::from_bits_truncate(error_code as u32);
    panic!("x86_64 page fault @ {addr:#x}: {flags:?}, frame={frame:#x?}");
}

extern "x86-interrupt" fn lapic_timer_handler(_frame: InterruptStackFrame) {
    timer_ack();
    irq::handle_irq(irq::systimer_irq());
}

extern "x86-interrupt" fn spurious_handler(_frame: InterruptStackFrame) {}

pub fn irq_local_enabled() -> bool {
    rflags::read().contains(rflags::RFlags::FLAGS_IF)
}

pub fn irq_local_set_enabled(enable: bool) {
    unsafe {
        if enable {
            x86::irq::enable();
        } else {
            x86::irq::disable();
        }
    }
}

pub fn current_cr3() -> PhysAddr {
    let raw = unsafe { controlregs::cr3() } as usize & !(page_size() - 1);
    raw.into()
}

pub fn set_cr3(addr: PhysAddr) {
    unsafe {
        controlregs::cr3_write(addr.raw() as u64);
    }
}

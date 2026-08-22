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

use crate::mem::page_size;

const IA32_EFER: u32 = 0xc000_0080;
const IA32_EFER_NXE: u64 = 1 << 11;

const PIT_CHANNEL2_PORT: u16 = 0x42;
const PIT_COMMAND_PORT: u16 = 0x43;
const PIT_CONTROL_PORT: u16 = 0x61;
const PIT_CHANNEL2_GATE: u8 = 0x01;
const PIT_SPEAKER_ENABLE: u8 = 0x02;
const PIT_CHANNEL2_OUT: u8 = 0x20;
const PIT_MODE0_CHANNEL2: u8 = 0xb0;
const PIT_TICK_RATE_HZ: u64 = 1_193_182;
const TSC_PIT_CALIBRATION_MS: u64 = 50;
const TSC_PIT_MAX_POLL_COUNT: usize = 5_000_000;
const MIN_VALID_TSC_FREQ_HZ: u64 = 10_000_000;
const MAX_VALID_TSC_FREQ_HZ: u64 = 10_000_000_000;

static TSC_FREQ_HZ: AtomicU64 = AtomicU64::new(0);
static HAS_INVARIANT_TSC: AtomicBool = AtomicBool::new(false);
static HAS_TSC_ADJUST: AtomicBool = AtomicBool::new(false);
static TSC_INFO_STATE: AtomicU8 = AtomicU8::new(0);
static TSC_ADJUST_REFERENCE_STATE: AtomicU8 = AtomicU8::new(0);
static TSC_ADJUST_REFERENCE: AtomicU64 = AtomicU64::new(0);
static TSC_ADJUST_CHANGED: AtomicBool = AtomicBool::new(false);
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
    enable_xsave_features();
    init_tsc_freq();
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

pub fn scheduler_counter_stability() -> crate::timer::CounterStability {
    let invariant_tsc = HAS_INVARIANT_TSC.load(Ordering::Acquire);
    let cpu_count = crate::smp::cpu_count();
    if !invariant_tsc || cpu_count != 1 {
        return crate::timer::CounterStability::Unstable;
    }
    classify_scheduler_counter(invariant_tsc, cpu_count, tsc_adjust_is_unchanged())
}

fn tsc_adjust_is_unchanged() -> bool {
    if !HAS_TSC_ADJUST.load(Ordering::Acquire) {
        return true;
    }

    let current_adjust = unsafe { rdmsr(msr::IA32_TSC_ADJUST) };
    if TSC_ADJUST_REFERENCE_STATE
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        TSC_ADJUST_REFERENCE.store(current_adjust, Ordering::Relaxed);
        TSC_ADJUST_REFERENCE_STATE.store(2, Ordering::Release);
    } else {
        while TSC_ADJUST_REFERENCE_STATE.load(Ordering::Acquire) != 2 {
            spin_loop();
        }
    }

    if current_adjust != TSC_ADJUST_REFERENCE.load(Ordering::Acquire) {
        TSC_ADJUST_CHANGED.store(true, Ordering::Release);
    }
    !TSC_ADJUST_CHANGED.load(Ordering::Acquire)
}

const fn classify_scheduler_counter(
    invariant_tsc: bool,
    cpu_count: usize,
    tsc_adjust_unchanged: bool,
) -> crate::timer::CounterStability {
    if invariant_tsc && cpu_count == 1 && tsc_adjust_unchanged {
        crate::timer::CounterStability::Stable
    } else {
        crate::timer::CounterStability::Unstable
    }
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
    let freq_hz = hypervisor_tsc_freq_hz(&cpuid)
        .or_else(|| cpuid_tsc_freq_hz(&cpuid))
        .or_else(pit_calibrate_tsc_freq_hz)
        .or_else(|| processor_base_freq_hz(&cpuid))
        .unwrap_or_else(|| {
            let fallback = 1_000_000_000u64;
            warn!("x86_64 TSC frequency unavailable, fallback to {fallback} Hz");
            fallback
        });
    let has_invariant_tsc = cpuid
        .get_advanced_power_mgmt_info()
        .is_some_and(|info| info.has_invariant_tsc());
    let has_tsc_adjust = cpuid
        .get_extended_feature_info()
        .is_some_and(|info| info.has_tsc_adjust_msr());

    HAS_INVARIANT_TSC.store(has_invariant_tsc, Ordering::Release);
    HAS_TSC_ADJUST.store(has_tsc_adjust, Ordering::Release);

    TSC_FREQ_HZ.store(freq_hz, Ordering::Release);
    TSC_INFO_STATE.store(2, Ordering::Release);
}

fn valid_tsc_freq_hz(freq: u64) -> Option<u64> {
    (MIN_VALID_TSC_FREQ_HZ..=MAX_VALID_TSC_FREQ_HZ)
        .contains(&freq)
        .then_some(freq)
}

fn hypervisor_tsc_freq_hz(cpuid: &CpuId) -> Option<u64> {
    cpuid
        .get_hypervisor_info()
        .and_then(|hv| hv.tsc_frequency())
        .map(|khz| khz as u64 * 1_000)
        .and_then(valid_tsc_freq_hz)
}

fn cpuid_tsc_freq_hz(cpuid: &CpuId) -> Option<u64> {
    cpuid
        .get_tsc_info()
        .and_then(|info| {
            if let Some(freq) = info.tsc_frequency().and_then(valid_tsc_freq_hz) {
                return Some(freq);
            }

            let numerator = info.numerator();
            let denominator = info.denominator();
            if numerator == 0 || denominator == 0 {
                return None;
            }

            let base_hz = processor_base_freq_hz(cpuid)? as u128;
            let crystal_hz = base_hz * denominator as u128 / numerator as u128;
            Some((crystal_hz * numerator as u128 / denominator as u128) as u64)
        })
        .and_then(valid_tsc_freq_hz)
}

fn processor_base_freq_hz(cpuid: &CpuId) -> Option<u64> {
    cpuid
        .get_processor_frequency_info()
        .map(|pinfo| pinfo.processor_base_frequency() as u64 * 1_000_000)
        .and_then(valid_tsc_freq_hz)
}

fn pit_calibrate_tsc_freq_hz() -> Option<u64> {
    let latch = ((PIT_TICK_RATE_HZ * TSC_PIT_CALIBRATION_MS) / 1_000) as u16;

    unsafe {
        let control = x86::io::inb(PIT_CONTROL_PORT);
        x86::io::outb(
            PIT_CONTROL_PORT,
            (control & !PIT_SPEAKER_ENABLE) | PIT_CHANNEL2_GATE,
        );
        x86::io::outb(PIT_COMMAND_PORT, PIT_MODE0_CHANNEL2);
        x86::io::outb(PIT_CHANNEL2_PORT, (latch & 0xff) as u8);
        x86::io::outb(PIT_CHANNEL2_PORT, (latch >> 8) as u8);
    }

    let start = ticks_now();
    let mut end = start;
    let mut done = false;
    for _ in 0..TSC_PIT_MAX_POLL_COUNT {
        if unsafe { x86::io::inb(PIT_CONTROL_PORT) } & PIT_CHANNEL2_OUT != 0 {
            end = ticks_now();
            done = true;
            break;
        }
        end = ticks_now();
        spin_loop();
    }

    if !done {
        return None;
    }

    end.wrapping_sub(start)
        .checked_mul(1_000)?
        .checked_div(TSC_PIT_CALIBRATION_MS)
        .and_then(valid_tsc_freq_hz)
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

    // The boot IDT carries only exception entries: the kernel's runtime IDT
    // owns every interrupt vector, and no interrupt can be delivered before
    // it is loaded because the boot path runs with interrupts disabled.
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

fn enable_nxe() {
    let efer = unsafe { rdmsr(IA32_EFER) } | IA32_EFER_NXE;
    unsafe {
        wrmsr(IA32_EFER, efer);
    }
}

/// Enable `CR4.OSXSAVE` and program `XCR0.{X87,SSE,AVX}` so userspace
/// (VEX-encoded) AVX instructions don't fault with `#UD` even when the CPU
/// reports `CPUID.01H:ECX.AVX`. Runs per-CPU from [`init_local`] (primary and,
/// via [`per_cpu_trap_init`], every secondary core — `XCR0` is per-core).
///
/// Everything is gated on `CPUID.01H:ECX.XSAVE` (bit 26): setting `CR4.OSXSAVE`
/// or executing `XSETBV` when XSAVE is unsupported `#GP`s, and the default
/// `qemu64` model has no XSAVE (so this is a no-op there). `OSXSAVE` must be set
/// before `XSETBV`; `X87` is mandatory and `SSE` must precede `AVX` in `XCR0`.
fn enable_xsave_features() {
    let Some(info) = CpuId::new().get_feature_info() else {
        return;
    };
    if !info.has_xsave() {
        return;
    }
    // SAFETY: XSAVE is supported (CPUID-checked above), so enabling CR4.OSXSAVE
    // and the subsequent XSETBV are well-defined and will not #GP.
    unsafe {
        controlregs::cr4_write(controlregs::cr4() | controlregs::Cr4::CR4_ENABLE_OS_XSAVE);
        let mut bits = controlregs::Xcr0::XCR0_FPU_MMX_STATE | controlregs::Xcr0::XCR0_SSE_STATE;
        if info.has_avx() {
            bits |= controlregs::Xcr0::XCR0_AVX_STATE;
        }
        controlregs::xcr0_write(bits);
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
        controlregs::cr3_write(addr.as_usize() as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::classify_scheduler_counter;
    use crate::timer::CounterStability;

    #[test]
    fn only_proven_single_cpu_invariant_tsc_uses_the_stable_path() {
        assert_eq!(
            classify_scheduler_counter(true, 1, true),
            CounterStability::Stable
        );
        assert_eq!(
            classify_scheduler_counter(false, 1, true),
            CounterStability::Unstable
        );
        assert_eq!(
            classify_scheduler_counter(true, 2, true),
            CounterStability::Unstable
        );
        assert_eq!(
            classify_scheduler_counter(true, 1, false),
            CounterStability::Unstable
        );
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[test]
    fn trap_constants_hold() {
        assert!(IA32_EFER == 0xc000_0080);
        assert!(IA32_EFER_NXE == (1 << 11));

        assert!(PIT_CHANNEL2_PORT == 0x42);
        assert!(PIT_COMMAND_PORT == 0x43);
        assert!(PIT_CONTROL_PORT == 0x61);
        assert!(PIT_CHANNEL2_GATE == 0x01);
        assert!(PIT_SPEAKER_ENABLE == 0x02);
        assert!(PIT_CHANNEL2_OUT == 0x20);
        assert!(PIT_MODE0_CHANNEL2 == 0xb0);
        assert!(PIT_TICK_RATE_HZ == 1_193_182);
        assert!(TSC_PIT_CALIBRATION_MS == 50);
        assert!(TSC_PIT_MAX_POLL_COUNT == 5_000_000);
        assert!(MIN_VALID_TSC_FREQ_HZ == 10_000_000);
        assert!(MAX_VALID_TSC_FREQ_HZ == 10_000_000_000);
    }
}

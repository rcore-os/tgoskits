use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

use acpi::{
    address::{AddressSpace, GenericAddress},
    sdt::fadt::Fadt,
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
use crate::mem::{page_size, phys_to_virt};

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
const PIT_CHANNEL2_PORT: u16 = 0x42;
const PIT_COMMAND_PORT: u16 = 0x43;
const PIT_CONTROL_PORT: u16 = 0x61;
const PIT_CHANNEL2_GATE: u8 = 0x01;
const PIT_SPEAKER_ENABLE: u8 = 0x02;
const PIT_CHANNEL2_OUT: u8 = 0x20;
const PIT_MODE0_CHANNEL2: u8 = 0xb0;
const PIT_TICK_RATE_HZ: u64 = 1_193_182;
const ACPI_PM_TIMER_HZ: u64 = 3_579_545;
const TSC_CALIBRATION_MS: u64 = 50;
const TSC_CALIBRATION_ROUNDS: usize = 3;
const TSC_CALIBRATION_MAX_POLL_COUNT: usize = 10_000_000;
const TSC_CALIBRATION_MAX_JITTER_PCT: u64 = 10;
const MIN_VALID_TSC_FREQ_HZ: u64 = 10_000_000;
const MAX_VALID_TSC_FREQ_HZ: u64 = 10_000_000_000;

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
    enable_xsave_features();
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
    let (freq_hz, source) = hypervisor_tsc_freq_hz(&cpuid)
        .map(|freq| (freq, "hypervisor"))
        .or_else(|| cpuid_tsc_freq_hz(&cpuid).map(|freq| (freq, "cpuid.15")))
        .or_else(|| acpi_pm_timer_calibrate_tsc_freq_hz().map(|freq| (freq, "acpi-pm-timer")))
        .or_else(|| pit_calibrate_tsc_freq_hz().map(|freq| (freq, "pit")))
        .or_else(|| processor_base_freq_hz(&cpuid).map(|freq| (freq, "cpuid-base")))
        .unwrap_or_else(|| {
            let fallback = 1_000_000_000u64;
            warn!("x86_64 TSC frequency unavailable, fallback to {fallback} Hz");
            (fallback, "fallback")
        });
    debug!("x86_64 TSC frequency from {source}: {freq_hz} Hz");
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
        .and_then(|info| info.tsc_frequency())
        .and_then(valid_tsc_freq_hz)
}

fn processor_base_freq_hz(cpuid: &CpuId) -> Option<u64> {
    cpuid
        .get_processor_frequency_info()
        .map(|pinfo| pinfo.processor_base_frequency() as u64 * 1_000_000)
        .and_then(valid_tsc_freq_hz)
}

struct AcpiPmTimer {
    gas: GenericAddress,
    mask: u32,
}

impl AcpiPmTimer {
    fn detect() -> Option<Self> {
        let tables = crate::acpi::tables().ok()?;
        let fadt_mapping = tables.find_tables::<Fadt>().next()?;
        let fadt = &*fadt_mapping;
        let gas = fadt.pm_timer_block().ok()??;
        if gas.bit_offset != 0 {
            return None;
        }
        match gas.address_space {
            AddressSpace::SystemIo if gas.address <= u16::MAX as u64 => {}
            AddressSpace::SystemMemory => {}
            _ => return None,
        }

        let mask = if { fadt.flags }.pm_timer_is_32_bit() {
            u32::MAX
        } else {
            0x00ff_ffff
        };
        Some(Self { gas, mask })
    }

    fn read(&self) -> Option<u32> {
        let value = match self.gas.address_space {
            AddressSpace::SystemIo => unsafe { x86::io::inl(self.gas.address as u16) },
            AddressSpace::SystemMemory => {
                let ptr = phys_to_virt(self.gas.address as usize).cast::<u32>();
                unsafe { ptr.read_volatile() }
            }
            _ => return None,
        };
        Some(value & self.mask)
    }
}

fn acpi_pm_timer_calibrate_tsc_freq_hz() -> Option<u64> {
    let timer = AcpiPmTimer::detect()?;
    let target_ticks = (ACPI_PM_TIMER_HZ * TSC_CALIBRATION_MS / 1_000) as u32;
    calibrated_tsc_freq_hz(|| timer.read(), timer.mask, target_ticks, ACPI_PM_TIMER_HZ)
}

fn pit_calibrate_tsc_freq_hz() -> Option<u64> {
    let mut samples = [0u64; TSC_CALIBRATION_ROUNDS];
    for sample in &mut samples {
        *sample = pit_calibrate_tsc_freq_sample_hz()?;
    }
    select_stable_calibration_sample(&samples)
}

fn pit_calibrate_tsc_freq_sample_hz() -> Option<u64> {
    let latch = ((PIT_TICK_RATE_HZ * TSC_CALIBRATION_MS) / 1_000) as u16;

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
    for _ in 0..TSC_CALIBRATION_MAX_POLL_COUNT {
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
        .checked_div(TSC_CALIBRATION_MS)
        .and_then(valid_tsc_freq_hz)
}

fn calibrated_tsc_freq_hz<F>(
    mut read_counter: F,
    counter_mask: u32,
    target_ticks: u32,
    counter_freq_hz: u64,
) -> Option<u64>
where
    F: FnMut() -> Option<u32>,
{
    let mut samples = [0u64; TSC_CALIBRATION_ROUNDS];
    for sample in &mut samples {
        *sample = calibrated_tsc_freq_sample_hz(
            &mut read_counter,
            counter_mask,
            target_ticks,
            counter_freq_hz,
        )?;
    }
    select_stable_calibration_sample(&samples)
}

fn calibrated_tsc_freq_sample_hz<F>(
    read_counter: &mut F,
    counter_mask: u32,
    target_ticks: u32,
    counter_freq_hz: u64,
) -> Option<u64>
where
    F: FnMut() -> Option<u32>,
{
    let start_counter = read_counter()?;
    let start_tsc = ticks_now();
    let mut end_tsc = start_tsc;
    let mut elapsed_counter = 0;
    let mut done = false;

    for _ in 0..TSC_CALIBRATION_MAX_POLL_COUNT {
        let now_counter = read_counter()?;
        elapsed_counter = now_counter.wrapping_sub(start_counter) & counter_mask;
        if elapsed_counter >= target_ticks {
            end_tsc = ticks_now();
            done = true;
            break;
        }
        spin_loop();
    }

    if !done || elapsed_counter == 0 {
        return None;
    }

    let elapsed_tsc = end_tsc.wrapping_sub(start_tsc);
    let freq = (elapsed_tsc as u128)
        .checked_mul(counter_freq_hz as u128)?
        .checked_div(elapsed_counter as u128)?;
    u64::try_from(freq).ok().and_then(valid_tsc_freq_hz)
}

fn select_stable_calibration_sample(samples: &[u64]) -> Option<u64> {
    let mut min = u64::MAX;
    let mut max = 0;
    for &sample in samples {
        min = min.min(sample);
        max = max.max(sample);
    }

    if min == u64::MAX {
        return None;
    }
    if max.saturating_sub(min) > min / 100 * TSC_CALIBRATION_MAX_JITTER_PCT {
        return None;
    }
    Some(min)
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
    unsafe { phys_to_virt(base).add(offset as usize) }.cast()
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

extern "x86-interrupt" fn lapic_timer_handler(_frame: InterruptStackFrame) {
    timer_ack();
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

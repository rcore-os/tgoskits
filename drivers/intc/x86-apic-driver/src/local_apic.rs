//! Per-CPU local APIC (xAPIC MMIO / x2APIC MSR) driver core.
//!
//! All register operations go through `x2apic::lapic::LocalApic`. Because the
//! `x2apic` API is `!Send`/`!Sync` and takes `&mut self`, this wrapper holds
//! only immutable configuration and builds a short-lived `LocalApic` per
//! operation on the caller's stack: every access then targets the *current*
//! CPU's local APIC (MSRs are per-CPU by definition and the xAPIC MMIO page is
//! identical on all CPUs), so the wrapper stays `Sync` with no interior
//! mutability and no lock in the EOI path. The per-operation cost is one
//! CPUID read, comparable to the per-operation `IA32_APIC_BASE` MSR read the
//! previous in-glue implementations paid for mode detection.
//!
//! A few operations the `x2apic` 0.5 public API cannot express are kept as a
//! private raw-access supplement at the bottom of this file. Each documents
//! its reason; they should move back to `x2apic` calls once upstream grows
//! the missing APIs.

use x2apic::lapic::{LocalApicBuilder, TimerDivide, TimerMode};

use crate::{ApicError, VirtAddr};

/// Local APIC mode selected by `IA32_APIC_BASE` bit 10.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApicMode {
    /// Classic MMIO-mapped local APIC.
    XApic,
    /// MSR-mapped x2APIC.
    X2Apic,
}

/// Static per-system local APIC configuration.
///
/// The vectors and timer settings are OS vector-space policy; the glue picks
/// them and passes the same values to every `X86LocalApic` it creates.
#[derive(Clone, Copy, Debug)]
pub struct LocalApicConfig {
    /// LVT vector for the local APIC timer interrupt.
    pub timer_vector: u8,
    /// LVT vector for the local APIC internal-error interrupt.
    ///
    /// `x2apic::LocalApic::enable` programs the LVT error entry, so the glue
    /// must dedicate a vector even if it never handles the interrupt.
    pub error_vector: u8,
    /// Spurious-interrupt vector programmed into the SVR.
    pub spurious_vector: u8,
    /// Timer mode programmed at bring-up (one-shot, periodic, or TSC
    /// deadline).
    pub timer_mode: TimerMode,
    /// Timer divide configuration programmed at bring-up.
    pub timer_divide: TimerDivide,
    /// Timer initial count programmed at bring-up. Zero keeps the timer
    /// from counting until the first deadline is armed.
    pub timer_initial: u32,
}

// Fixed IPI level bit (Intel SDM Vol. 3, "Interrupt Command Register"),
// matching the encoding the runtime IPI path has always used, plus the
// "self" destination shorthand (bits 19:18 = 01b) for the xAPIC fallback.
const ICR_FIXED_LEVEL: u32 = 0x0000_4000;
const ICR_DEST_SELF: u32 = 0x0004_0000;

const IPI_DELIVERY_WAIT_SPINS: usize = 1_000_000;
const LVT_MASKED: u32 = 1 << 16;

/// OS-facing local APIC driver.
///
/// The instance is plain configuration plus the mapped xAPIC MMIO page, so it
/// can live in a global static and be shared across CPUs. Hardware state is
/// only touched through per-operation [`x2apic::lapic::LocalApic`]
/// instances.
pub struct X86LocalApic {
    config: LocalApicConfig,
    xapic_mmio_base: VirtAddr,
}

impl X86LocalApic {
    /// Creates a local APIC driver handle without touching hardware.
    ///
    /// ```compile_fail
    /// use x86_apic_driver::{LocalApicConfig, VirtAddr, X86LocalApic};
    ///
    /// fn construct_from_unverified_address(config: LocalApicConfig, base: VirtAddr) {
    ///     let _ = X86LocalApic::new(config, base);
    /// }
    /// ```
    ///
    /// # Safety
    ///
    /// `xapic_mmio_base` must point to a valid, device-mapped kernel mapping
    /// of the complete physical local APIC page reported by
    /// `IA32_APIC_BASE`. The mapping must remain valid for every use of the
    /// returned handle. It is dereferenced only while the current CPU runs in
    /// xAPIC mode.
    pub unsafe fn new(config: LocalApicConfig, xapic_mmio_base: VirtAddr) -> Self {
        Self {
            config,
            xapic_mmio_base,
        }
    }

    /// Brings up the local APIC on the **current** CPU.
    ///
    /// Enables the APIC (global-enable bit plus the x2APIC MSR bit when the
    /// CPU supports x2APIC), programs the timer/error/spurious vectors, the
    /// timer mode/divide/initial count, and leaves the timer and both local
    /// interrupt pins masked. This must run once per CPU, with interrupts
    /// disabled.
    ///
    /// # Safety
    ///
    /// The caller must ensure the current CPU's APIC may safely be reprogrammed
    /// (early boot, interrupts disabled, no interrupt sources routed to the
    /// vectors in `config` yet).
    pub unsafe fn bring_up(&self) -> Result<(), ApicError> {
        // `x2apic::LocalApic::enable` only manages the x2APIC bit; the global
        // APIC-enable bit must be set before any register access.
        unsafe {
            set_apic_base_enable_bit();
        }

        // Preserve firmware delivery configuration before `enable` invokes
        // x2apic 0.5's pin-disable helper, which writes both entries as zero
        // and therefore clears their mask bits as well.
        let (lint0, lint1) = unsafe { read_local_interrupt_pins(self.xapic_mmio_base) };

        let mut lapic = self.instance();
        unsafe {
            lapic.enable();
            let (readback_lint0, readback_lint1) =
                mask_local_interrupt_pins(self.xapic_mmio_base, lint0, lint1);
            // `enable` leaves the timer unmasked; restore the masked-at-boot
            // invariant so the first deadline arms it explicitly.
            lapic.disable_timer();
            if !local_interrupt_pins_are_masked(readback_lint0, readback_lint1) {
                return Err(ApicError::LocalInterruptPinsUnmasked {
                    lint0: readback_lint0,
                    lint1: readback_lint1,
                });
            }
        }
        Ok(())
    }

    /// Signals end-of-interrupt for the current CPU's most recent interrupt.
    pub fn eoi(&self) {
        let mut lapic = self.instance();
        unsafe {
            lapic.end_of_interrupt();
        }
    }

    /// Returns the local APIC id of the current CPU.
    pub fn apic_id(&self) -> u32 {
        let lapic = self.instance();
        unsafe { lapic.id() }
    }

    /// Sends a fixed IPI with `vector` to the CPU whose APIC id is
    /// `dest_apic_id`, then waits for delivery to complete.
    ///
    /// Fails with [`ApicError::XapicDestinationOverflow`] on xAPIC systems
    /// whose destination id does not fit the 8-bit ICR-high field.
    pub fn send_fixed_ipi(&self, dest_apic_id: u32, vector: u8) -> Result<(), ApicError> {
        match current_apic_mode() {
            // x2apic encodes the destination in ICR bits 63:32, which is the
            // architectural x2APIC layout.
            ApicMode::X2Apic => {
                let mut lapic = self.instance();
                unsafe {
                    lapic.send_ipi(vector, dest_apic_id);
                }
            }
            // x2apic 0.5 writes the destination into ICR_HIGH without the
            // shift into bits 31:24 that the xAPIC destination field
            // requires, so the xAPIC path uses the raw encoding.
            ApicMode::XApic => {
                let destination = xapic_destination(dest_apic_id)?;
                let icr_low = ICR_FIXED_LEVEL | u32::from(vector);
                unsafe {
                    write_xapic_icr(self.xapic_mmio_base, destination, icr_low);
                }
            }
        }
        self.wait_ipi_delivery()
    }

    /// Sends a fixed self-IPI with `vector` to the current CPU.
    ///
    /// Uses the x2APIC self-IPI register when available (delivery is
    /// immediate by definition, so no delivery wait applies) and the xAPIC
    /// self-shorthand ICR otherwise, followed by a delivery wait.
    pub fn send_self_ipi(&self, vector: u8) -> Result<(), ApicError> {
        if current_apic_mode() == ApicMode::X2Apic {
            let mut lapic = self.instance();
            unsafe {
                lapic.send_ipi_self(vector);
            }
            return Ok(());
        }
        let icr_low = ICR_FIXED_LEVEL | ICR_DEST_SELF | u32::from(vector);
        unsafe {
            clear_esr(self.xapic_mmio_base);
            write_xapic_icr(self.xapic_mmio_base, 0, icr_low);
        }
        self.wait_ipi_delivery()
    }

    /// Masks or unmasks the LVT timer entry on the current CPU.
    pub fn timer_set_masked(&self, masked: bool) {
        let mut lapic = self.instance();
        unsafe {
            if masked {
                lapic.disable_timer();
            } else {
                lapic.enable_timer();
            }
        }
    }

    /// Returns whether the LVT timer entry is currently unmasked.
    pub fn timer_is_unmasked(&self) -> bool {
        let lvt = unsafe { read_lvt_timer(self.xapic_mmio_base) };
        lvt & LVT_MASKED == 0
    }

    /// Sets the timer initial count (one-shot and periodic modes).
    pub fn timer_set_initial_count(&self, initial: u32) {
        let mut lapic = self.instance();
        unsafe {
            lapic.set_timer_initial(initial);
        }
    }

    /// Arms the TSC-deadline timer with an absolute TSC value (Intel SDM
    /// Vol. 3, `IA32_TSC_DEADLINE`). Only meaningful when the timer was
    /// brought up in [`TimerMode::TscDeadline`].
    pub fn timer_set_tsc_deadline(&self, deadline: u64) {
        unsafe {
            x86::msr::wrmsr(x86::msr::IA32_TSC_DEADLINE, deadline);
        }
    }

    /// Reads the timer current count.
    pub fn timer_current_count(&self) -> u32 {
        let lapic = self.instance();
        unsafe { lapic.timer_current() }
    }

    /// Spins until the last IPI has been accepted by the local APIC, matching
    /// the delivery-status wait the previous in-glue implementations
    /// performed after every ICR write.
    fn wait_ipi_delivery(&self) -> Result<(), ApicError> {
        let lapic = self.instance();
        for _ in 0..IPI_DELIVERY_WAIT_SPINS {
            if !unsafe { lapic.get_ipi_delivery_status() } {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(ApicError::IpiDeliveryTimeout)
    }

    /// Builds a fresh `x2apic` handle for one register operation.
    ///
    /// Building reads CPUID and initializes plain struct fields only; it
    /// performs no register writes, so per-operation construction is safe in
    /// interrupt context.
    fn instance(&self) -> x2apic::lapic::LocalApic {
        let mut builder = LocalApicBuilder::new();
        builder
            .timer_vector(usize::from(self.config.timer_vector))
            .error_vector(usize::from(self.config.error_vector))
            .spurious_vector(usize::from(self.config.spurious_vector))
            .timer_mode(self.config.timer_mode)
            .timer_divide(self.config.timer_divide)
            .timer_initial(self.config.timer_initial)
            .set_xapic_base(self.xapic_mmio_base.as_usize() as u64);
        // The builder only fails when xAPIC mode has no MMIO base or a
        // required vector is missing; construction always supplies both.
        builder
            .build()
            .expect("xapic MMIO base and all required vectors are always set")
    }
}

/// Returns the physical local APIC base page address from `IA32_APIC_BASE`.
///
/// The glue maps this address with its own `phys_to_virt` and passes the
/// result to [`X86LocalApic::new`]; the driver never maps memory itself.
pub fn apic_phys_base() -> usize {
    (unsafe { x86::msr::rdmsr(IA32_APIC_BASE) } & IA32_APIC_BASE_PAGE_MASK) as usize
}

/// Encodes an xAPIC destination into the ICR-high 8-bit destination field,
/// rejecting ids the field cannot represent instead of truncating them.
fn xapic_destination(apic_id: u32) -> Result<u32, ApicError> {
    let dest = u8::try_from(apic_id).map_err(|_| ApicError::XapicDestinationOverflow(apic_id))?;
    Ok(u32::from(dest) << 24)
}

/// Returns whether the CPU supports the TSC-deadline LVT timer mode
/// (`CPUID.01H:ECX.TSC_Deadline`).
pub fn cpu_has_tsc_deadline() -> bool {
    x86::cpuid::CpuId::new()
        .get_feature_info()
        .is_some_and(|info| info.has_tsc_deadline())
}

/// Returns the local APIC mode currently selected by `IA32_APIC_BASE`.
fn current_apic_mode() -> ApicMode {
    let base = unsafe { x86::msr::rdmsr(IA32_APIC_BASE) };
    if base & IA32_APIC_BASE_X2APIC_ENABLE != 0 {
        ApicMode::X2Apic
    } else {
        ApicMode::XApic
    }
}

// --- raw-access supplement ---------------------------------------------------
//
// `x2apic` 0.5 has no public API for the operations below. Each helper is the
// minimum volatile/MSR access for it, kept private; replace the call sites
// with `x2apic` calls once upstream grows the API:
// - ESR write (clearing): only `error_flags()` (a read) is public.
// - Raw ICR values: `IpiAllShorthand` has no self-only shorthand, and the
//   xAPIC write path drops the destination shift into ICR_HIGH bits 31:24,
//   so fixed and self IPIs use the raw encoding there as well (QEMU
//   `hw/intc/apic.c` reads the destination from `icr[1] >> 24`, matching
//   Intel SDM Vol. 3).
// - LVT timer read: needed to observe the timer mask bit.
// - LVT LINT0/LINT1 read/write: `enable()` claims to mask both pins but
//   x2apic 0.5 writes zero and therefore clears the architectural mask bit.
// - `IA32_APIC_BASE` bit 11: `enable()` only manages the x2APIC bit.

const IA32_APIC_BASE: u32 = 0x1b;
const IA32_APIC_BASE_ENABLE: u64 = 1 << 11;
const IA32_APIC_BASE_X2APIC_ENABLE: u64 = 1 << 10;
const IA32_APIC_BASE_PAGE_MASK: u64 = 0xffff_f000;

// xAPIC MMIO register offsets (bytes) within the local APIC page.
const XAPIC_REG_ESR: u32 = 0x280;
const XAPIC_REG_ICR_LOW: u32 = 0x300;
const XAPIC_REG_ICR_HIGH: u32 = 0x310;
const XAPIC_REG_LVT_TIMER: u32 = 0x320;
const XAPIC_REG_LVT_LINT0: u32 = 0x350;
const XAPIC_REG_LVT_LINT1: u32 = 0x360;

// x2APIC MSR addresses.
const X2APIC_ESR: u32 = 0x828;
const X2APIC_LVT_TIMER: u32 = 0x832;
const X2APIC_LVT_LINT0: u32 = 0x835;
const X2APIC_LVT_LINT1: u32 = 0x836;

/// Sets the global APIC-enable bit in `IA32_APIC_BASE`.
unsafe fn set_apic_base_enable_bit() {
    let base = unsafe { x86::msr::rdmsr(IA32_APIC_BASE) } | IA32_APIC_BASE_ENABLE;
    unsafe {
        x86::msr::wrmsr(IA32_APIC_BASE, base);
    }
}

/// Clears the error status register by writing zero, the documented
/// clear-to-zero semantics of the ESR.
unsafe fn clear_esr(base: VirtAddr) {
    unsafe {
        match current_apic_mode() {
            ApicMode::X2Apic => x86::msr::wrmsr(X2APIC_ESR, 0),
            ApicMode::XApic => mmio_write(base, XAPIC_REG_ESR, 0),
        }
    }
}

/// Writes an xAPIC ICR: destination field to ICR-high first, command to
/// ICR-low second. x86 TSO orders earlier write-back stores before the APIC's
/// UC MMIO doorbell, so no additional publication fence is required.
unsafe fn write_xapic_icr(base: VirtAddr, destination: u32, icr_low: u32) {
    unsafe {
        mmio_write(base, XAPIC_REG_ICR_HIGH, destination);
        mmio_write(base, XAPIC_REG_ICR_LOW, icr_low);
    }
}

/// Reads the LVT timer entry through whichever register space is active.
unsafe fn read_lvt_timer(base: VirtAddr) -> u32 {
    match current_apic_mode() {
        ApicMode::X2Apic => unsafe { x86::msr::rdmsr(X2APIC_LVT_TIMER) as u32 },
        ApicMode::XApic => unsafe { mmio_read(base, XAPIC_REG_LVT_TIMER) },
    }
}

/// Masks both local interrupt pins while preserving their vector and delivery
/// configuration.
unsafe fn mask_local_interrupt_pins(base: VirtAddr, lint0: u32, lint1: u32) -> (u32, u32) {
    let (lint0, lint1) = masked_local_interrupt_pins(lint0, lint1);
    unsafe {
        write_lvt(base, XAPIC_REG_LVT_LINT0, X2APIC_LVT_LINT0, lint0);
        write_lvt(base, XAPIC_REG_LVT_LINT1, X2APIC_LVT_LINT1, lint1);
    }
    unsafe { read_local_interrupt_pins(base) }
}

unsafe fn read_local_interrupt_pins(base: VirtAddr) -> (u32, u32) {
    let lint0 = unsafe { read_lvt(base, XAPIC_REG_LVT_LINT0, X2APIC_LVT_LINT0) };
    let lint1 = unsafe { read_lvt(base, XAPIC_REG_LVT_LINT1, X2APIC_LVT_LINT1) };
    (lint0, lint1)
}

fn masked_local_interrupt_pins(lint0: u32, lint1: u32) -> (u32, u32) {
    (lint0 | LVT_MASKED, lint1 | LVT_MASKED)
}

fn local_interrupt_pins_are_masked(lint0: u32, lint1: u32) -> bool {
    lint0 & LVT_MASKED != 0 && lint1 & LVT_MASKED != 0
}

unsafe fn read_lvt(base: VirtAddr, xapic_offset: u32, x2apic_msr: u32) -> u32 {
    match current_apic_mode() {
        ApicMode::X2Apic => unsafe { x86::msr::rdmsr(x2apic_msr) as u32 },
        ApicMode::XApic => unsafe { mmio_read(base, xapic_offset) },
    }
}

unsafe fn write_lvt(base: VirtAddr, xapic_offset: u32, x2apic_msr: u32, value: u32) {
    unsafe {
        match current_apic_mode() {
            ApicMode::X2Apic => x86::msr::wrmsr(x2apic_msr, u64::from(value)),
            ApicMode::XApic => mmio_write(base, xapic_offset, value),
        }
    }
}

unsafe fn mmio_read(base: VirtAddr, offset: u32) -> u32 {
    unsafe {
        base.as_ptr::<u32>()
            .byte_add(offset as usize)
            .read_volatile()
    }
}

unsafe fn mmio_write(base: VirtAddr, offset: u32, value: u32) {
    unsafe {
        base.as_ptr::<u32>()
            .byte_add(offset as usize)
            .write_volatile(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xapic_destination_rejects_high_apic_ids_without_truncation() {
        assert_eq!(xapic_destination(0xfe), Ok(0xfe00_0000));
        assert_eq!(
            xapic_destination(0x100),
            Err(ApicError::XapicDestinationOverflow(0x100))
        );
    }

    #[test]
    fn fixed_ipi_level_bit_matches_the_runtime_icr_encoding() {
        assert_eq!(ICR_FIXED_LEVEL, 0x4000);
    }

    #[test]
    fn local_interrupt_pin_masking_sets_both_masks_without_clobbering_configuration() {
        // Typical firmware delivery modes: ExtINT on LINT0 and NMI on LINT1.
        let lint0 = 0x700;
        let lint1 = 0x455;

        let (masked_lint0, masked_lint1) = masked_local_interrupt_pins(lint0, lint1);

        assert_eq!(masked_lint0, lint0 | LVT_MASKED);
        assert_eq!(masked_lint1, lint1 | LVT_MASKED);
        assert!(local_interrupt_pins_are_masked(masked_lint0, masked_lint1));
        assert!(!local_interrupt_pins_are_masked(
            masked_lint0 & !LVT_MASKED,
            masked_lint1
        ));
    }
}

//! I/O APIC chip driver core.
//!
//! Redirection-table programming goes through `x2apic::ioapic::IoApic`; this
//! module adds the chip identity, GSI range bookkeeping, and the masked
//! routing policy the OS glue relies on. Device discovery (ACPI MADT) and the
//! address mapping stay in the glue, mirroring how `arm-gic-driver` receives
//! pre-mapped register bases.

use x2apic::ioapic::{IoApic, IrqFlags, IrqMode};

use crate::{ApicError, VirtAddr};

/// Identity and location of one I/O APIC chip.
///
/// `phys_address` is the ACPI-reported physical address used to match MADT
/// GSI routes against this chip; `mmio_base` is the glue's kernel mapping of
/// that address, which is the only one the driver dereferences.
#[derive(Clone, Copy, Debug)]
pub struct IoApicInfo {
    /// ACPI I/O APIC id (`MADT Type 2::I/O APIC ID`).
    pub id: u8,
    /// Physical address reported by ACPI, kept for route identity matching.
    pub phys_address: u64,
    /// Kernel mapping of the I/O APIC register page.
    pub mmio_base: VirtAddr,
    /// First global system interrupt served by this chip.
    pub gsi_base: u32,
}

/// Trigger mode for an interrupt line, mapped onto the redirection-entry
/// trigger flag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriggerMode {
    Edge,
    Level,
}

/// Pin polarity for an interrupt line, mapped onto the redirection-entry
/// polarity flag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinPolarity {
    ActiveHigh,
    ActiveLow,
}

/// Translates ACPI-style trigger/polarity into `x2apic` redirection-entry
/// flags.
pub fn intx_flags(trigger: TriggerMode, polarity: PinPolarity) -> IrqFlags {
    let mut flags = IrqFlags::empty();
    if trigger == TriggerMode::Level {
        flags |= IrqFlags::LEVEL_TRIGGERED;
    }
    if polarity == PinPolarity::ActiveLow {
        flags |= IrqFlags::LOW_ACTIVE;
    }
    flags
}

/// One I/O APIC chip with its redirection table masked at construction.
///
/// The chip starts with every redirection entry masked and programmed with a
/// placeholder vector, so stray lines cannot raise interrupts until the OS
/// glue routes and enables them explicitly.
pub struct X86IoApic {
    info: IoApicInfo,
    redirection_entries: u32,
    ioapic: IoApic,
}

impl X86IoApic {
    /// Probes and masks one I/O APIC chip.
    ///
    /// # Safety
    ///
    /// `info.mmio_base` must be the mapped I/O APIC register page for the
    /// chip described by `info`, and no other code may race the redirection
    /// table during the probe.
    pub unsafe fn new(info: IoApicInfo) -> Self {
        const MASKED_PLACEHOLDER_VECTOR: u8 = 0x21;

        let mut ioapic = unsafe { IoApic::new(info.mmio_base.as_usize() as u64) };
        let max_entry = unsafe { ioapic.max_table_entry() };
        let redirection_entries = u32::from(max_entry.saturating_add(1));

        unsafe {
            ioapic.init(MASKED_PLACEHOLDER_VECTOR);
            for input in 0..=max_entry {
                let mut entry = ioapic.table_entry(input);
                entry.set_flags(entry.flags() | IrqFlags::MASKED);
                ioapic.set_table_entry(input, entry);
            }
        }

        log::info!(
            "I/O APIC initialized: id={} base={:#x} gsi_base={} entries={}",
            info.id,
            info.phys_address,
            info.gsi_base,
            redirection_entries
        );

        Self {
            info,
            redirection_entries,
            ioapic,
        }
    }

    /// Returns the chip identity and location.
    pub fn info(&self) -> &IoApicInfo {
        &self.info
    }

    /// Returns the number of redirection entries reported by the chip.
    pub fn redirection_entries(&self) -> u32 {
        self.redirection_entries
    }

    /// Returns whether `gsi` falls inside this chip's GSI range.
    pub fn contains_gsi(&self, gsi: u32) -> bool {
        let start = self.info.gsi_base;
        let end = start.saturating_add(self.redirection_entries);
        (start..end).contains(&gsi)
    }

    /// Programs redirection entry `input` with `vector`, trigger, polarity,
    /// and destination, leaving it masked.
    ///
    /// Fails with [`ApicError::InvalidIoApicInput`] when `input` is outside
    /// the redirection table.
    pub fn program_input_masked(
        &mut self,
        input: u8,
        vector: u8,
        trigger: TriggerMode,
        polarity: PinPolarity,
        destination: u8,
    ) -> Result<(), ApicError> {
        self.check_input(input)?;
        unsafe {
            let mut entry = self.ioapic.table_entry(input);
            entry.set_vector(vector);
            entry.set_mode(IrqMode::Fixed);
            entry.set_flags(intx_flags(trigger, polarity) | IrqFlags::MASKED);
            entry.set_dest(destination);
            self.ioapic.set_table_entry(input, entry);
        }
        Ok(())
    }

    /// Unmasks redirection entry `input`, completing the enable sequence that
    /// starts with [`X86IoApic::program_input_masked`].
    pub fn unmask_input(&mut self, input: u8) -> Result<(), ApicError> {
        self.check_input(input)?;
        unsafe {
            self.ioapic.enable_irq(input);
        }
        Ok(())
    }

    /// Reprograms only the destination field of redirection entry `input`.
    pub fn set_input_destination(&mut self, input: u8, destination: u8) -> Result<(), ApicError> {
        self.check_input(input)?;
        unsafe {
            let mut entry = self.ioapic.table_entry(input);
            entry.set_dest(destination);
            self.ioapic.set_table_entry(input, entry);
        }
        Ok(())
    }

    fn check_input(&self, input: u8) -> Result<(), ApicError> {
        if u32::from(input) < self.redirection_entries {
            Ok(())
        } else {
            Err(ApicError::InvalidIoApicInput(input))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intx_flags_preserve_trigger_and_polarity() {
        let level_low = intx_flags(TriggerMode::Level, PinPolarity::ActiveLow);
        assert!(level_low.contains(IrqFlags::LEVEL_TRIGGERED));
        assert!(level_low.contains(IrqFlags::LOW_ACTIVE));

        let edge_high = intx_flags(TriggerMode::Edge, PinPolarity::ActiveHigh);
        assert!(!edge_high.contains(IrqFlags::LEVEL_TRIGGERED));
        assert!(!edge_high.contains(IrqFlags::LOW_ACTIVE));
    }

    #[test]
    fn gsi_range_spans_the_reported_redirection_entries() {
        // `IoApic::new` only records the (non-null) base address without
        // touching the register page, so an inert instance plus injected
        // field values are enough to verify range bookkeeping on the host.
        let chip = X86IoApic {
            info: IoApicInfo {
                id: 0,
                phys_address: 0xfec0_0000,
                mmio_base: VirtAddr::new(0),
                gsi_base: 16,
            },
            redirection_entries: 24,
            ioapic: unsafe { IoApic::new(0xdead_0000) },
        };

        assert!(!chip.contains_gsi(15));
        assert!(chip.contains_gsi(16));
        assert!(chip.contains_gsi(39));
        assert!(!chip.contains_gsi(40));
    }
}

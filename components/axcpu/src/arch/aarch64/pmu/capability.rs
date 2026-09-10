//! Per-CPU PMUv3 capabilities, following Linux arm_pmuv3.c probe semantics.

/// Architectural evidence for an event encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventSupport {
    /// The architecture reports the event as implemented.
    Supported,
    /// The architecture reports the event as absent.
    Unsupported,
    /// The encoding is outside the architectural identification bitmaps.
    ImplementationDefined,
}

/// Immutable capabilities observed on one CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PmuInfo {
    /// ID_AA64DFR0_EL1.PMUVer encoding.
    pub version: u8,
    /// Number of programmable counters, excluding fixed cycle/instruction counters.
    pub num_counters: usize,
    /// ID_AA64DFR1_EL1.PMICNTR reports a dedicated instruction counter.
    /// This is independent of the PMUVer encoding.
    pub has_instruction_counter: bool,
    /// Currently configured programmable-counter overflow width.
    pub counter_width: u8,
    /// Currently configured cycle-counter overflow width.
    pub cycle_counter_width: u8,
    /// Full PMCEID0, including the extended common-event bits.
    pub pmceid0: u64,
    /// Full PMCEID1, including the extended common-event bits.
    pub pmceid1: u64,
}

impl PmuInfo {
    /// Reports PMUv3p5 long programmable-counter support.
    pub const fn has_long_counters(self) -> bool {
        self.version >= 6
    }

    /// Queries the common and extended common-event identification bitmaps.
    pub const fn event_support(self, event: u16) -> EventSupport {
        let (bitmap, bit) = match event {
            0x0000..=0x001f => (self.pmceid0, event),
            0x0020..=0x003f => (self.pmceid1, event - 0x20),
            0x4000..=0x401f => (self.pmceid0, event - 0x4000 + 32),
            0x4020..=0x403f => (self.pmceid1, event - 0x4020 + 32),
            _ => return EventSupport::ImplementationDefined,
        };
        if bitmap & (1u64 << bit) != 0 {
            EventSupport::Supported
        } else {
            EventSupport::Unsupported
        }
    }
}

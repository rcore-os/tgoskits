use crate::{
    X86AccessWidth, X86GuestPhysAddr, X86GuestPhysAddrRange, X86VlapicError, X86VlapicResult,
    lock::SpinMutex as Mutex,
};

const IOAPIC_BASE: usize = 0xfec0_0000;
const IOAPIC_SIZE: usize = 0x1000;

const IOREGSEL: usize = 0x00;
const IOWIN: usize = 0x10;

const IOAPIC_ID: u32 = 0x00;
const IOAPIC_VER: u32 = 0x01;
const IOAPIC_ARB: u32 = 0x02;
const IOREDTBL_BASE: u32 = 0x10;

const IOAPIC_ID_VALUE: u32 = 1 << 24;
const IOAPIC_VERSION_VALUE: u32 = 0x11 | ((MAX_REDIRECTION_ENTRY as u32) << 16);
const MAX_REDIRECTION_ENTRY: usize = 23;
const REDIRECTION_ENTRY_COUNT: usize = MAX_REDIRECTION_ENTRY + 1;
const REDIRECTION_ENTRY_MASKED: u64 = 1 << 16;
const REDIRECTION_ENTRY_TRIGGER_MODE: u64 = 1 << 15;
const REDIRECTION_ENTRY_REMOTE_IRR: u64 = 1 << 14;
const REDIRECTION_ENTRY_DELIVERY_MODE_MASK: u64 = 0b111 << 8;

#[derive(Debug)]
struct IoApicState {
    selector: u32,
    redirection_table: [u64; REDIRECTION_ENTRY_COUNT],
    pending_level: [bool; REDIRECTION_ENTRY_COUNT],
}

impl IoApicState {
    const fn new() -> Self {
        Self {
            selector: 0,
            redirection_table: [REDIRECTION_ENTRY_MASKED; REDIRECTION_ENTRY_COUNT],
            pending_level: [false; REDIRECTION_ENTRY_COUNT],
        }
    }

    fn interrupt_for_entry(&mut self, gsi: usize) -> Option<IoApicInterrupt> {
        let entry = self.redirection_table.get_mut(gsi)?;
        if *entry & REDIRECTION_ENTRY_MASKED != 0 {
            return None;
        }

        if *entry & REDIRECTION_ENTRY_DELIVERY_MODE_MASK != 0 {
            debug!("vIOAPIC GSI {gsi} uses unsupported delivery mode entry {entry:#x}");
            return None;
        }

        let vector = (*entry & 0xff) as u8;
        if vector < 16 {
            return None;
        }

        let level_triggered = *entry & REDIRECTION_ENTRY_TRIGGER_MODE != 0;
        if level_triggered {
            if *entry & REDIRECTION_ENTRY_REMOTE_IRR != 0 {
                self.pending_level[gsi] = true;
                return None;
            }
            *entry |= REDIRECTION_ENTRY_REMOTE_IRR;
        }

        Some(IoApicInterrupt {
            vector,
            level_triggered,
        })
    }

    fn end_of_interrupt(&mut self, vector: u8) -> Option<IoApicEoi> {
        for gsi in 0..REDIRECTION_ENTRY_COUNT {
            let matched = {
                let entry = &mut self.redirection_table[gsi];
                if (*entry & 0xff) as u8 != vector
                    || *entry & REDIRECTION_ENTRY_TRIGGER_MODE == 0
                    || *entry & REDIRECTION_ENTRY_REMOTE_IRR == 0
                {
                    false
                } else {
                    *entry &= !REDIRECTION_ENTRY_REMOTE_IRR;
                    true
                }
            };
            if !matched {
                continue;
            }

            let pending = core::mem::take(&mut self.pending_level[gsi])
                .then(|| self.interrupt_for_entry(gsi))
                .flatten();
            return Some(IoApicEoi { gsi, pending });
        }

        None
    }
}

/// A routed interrupt from the virtual IO APIC.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IoApicInterrupt {
    /// Guest interrupt vector.
    pub vector: u8,
    /// Whether the redirection entry is level-triggered.
    pub level_triggered: bool,
}

/// Result of a virtual IO APIC EOI broadcast.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IoApicEoi {
    /// The GSI whose remote-IRR state was cleared.
    pub gsi: usize,
    /// A deferred level-triggered interrupt that should be injected now.
    pub pending: Option<IoApicInterrupt>,
}

/// A minimal emulated x86 IO APIC.
pub struct EmulatedIoApic {
    base: X86GuestPhysAddr,
    size: usize,
    state: Mutex<IoApicState>,
}

impl EmulatedIoApic {
    /// Create a new `EmulatedIoApic`.
    pub fn new(base: X86GuestPhysAddr, size: Option<usize>) -> Self {
        Self {
            base,
            size: size.unwrap_or(IOAPIC_SIZE),
            state: Mutex::new(IoApicState::new()),
        }
    }

    /// Create an IO APIC at the default PC-compatible GPA.
    pub fn new_default() -> Self {
        Self::new(X86GuestPhysAddr::from_usize(IOAPIC_BASE), Some(IOAPIC_SIZE))
    }

    /// Return the guest interrupt vector programmed for a GSI.
    pub fn vector_for_gsi(&self, gsi: usize) -> Option<u8> {
        let state = self.state.lock();
        let entry = *state.redirection_table.get(gsi)?;
        if entry & REDIRECTION_ENTRY_MASKED != 0 {
            return None;
        }

        if entry & REDIRECTION_ENTRY_DELIVERY_MODE_MASK != 0 {
            debug!("vIOAPIC GSI {gsi} uses unsupported delivery mode entry {entry:#x}");
            return None;
        }

        let vector = (entry & 0xff) as u8;
        if vector < 16 {
            return None;
        }

        Some(vector)
    }

    /// Assert an IO APIC input line and return the interrupt to inject.
    pub fn assert_gsi(&self, gsi: usize) -> Option<IoApicInterrupt> {
        let mut state = self.state.lock();
        state.interrupt_for_entry(gsi)
    }

    /// Process an EOI broadcast from the local APIC.
    pub fn end_of_interrupt(&self, vector: u8) -> Option<IoApicEoi> {
        let mut state = self.state.lock();
        state.end_of_interrupt(vector)
    }

    fn offset(&self, addr: X86GuestPhysAddr) -> usize {
        addr.as_usize() - self.base.as_usize()
    }

    fn read_selected_register(state: &IoApicState) -> X86VlapicResult<u32> {
        match state.selector {
            IOAPIC_ID => Ok(IOAPIC_ID_VALUE),
            IOAPIC_VER => Ok(IOAPIC_VERSION_VALUE),
            IOAPIC_ARB => Ok(IOAPIC_ID_VALUE),
            reg @ IOREDTBL_BASE..=0x3f => {
                let index = ((reg - IOREDTBL_BASE) / 2) as usize;
                if index >= REDIRECTION_ENTRY_COUNT {
                    return Err(X86VlapicError::InvalidInput);
                }
                let entry = state.redirection_table[index];
                if (reg - IOREDTBL_BASE) & 1 == 0 {
                    Ok(entry as u32)
                } else {
                    Ok((entry >> 32) as u32)
                }
            }
            reg => {
                debug!("vIOAPIC read from unsupported register {reg:#x}");
                Ok(0)
            }
        }
    }

    fn write_selected_register(state: &mut IoApicState, value: u32) -> X86VlapicResult {
        match state.selector {
            IOAPIC_ID | IOAPIC_VER | IOAPIC_ARB => Ok(()),
            reg @ IOREDTBL_BASE..=0x3f => {
                let index = ((reg - IOREDTBL_BASE) / 2) as usize;
                if index >= REDIRECTION_ENTRY_COUNT {
                    return Err(X86VlapicError::InvalidInput);
                }
                let entry = &mut state.redirection_table[index];
                if (reg - IOREDTBL_BASE) & 1 == 0 {
                    let old_low = *entry & !REDIRECTION_ENTRY_REMOTE_IRR & 0xffff_ffff;
                    let new_low = (value as u64) & !REDIRECTION_ENTRY_REMOTE_IRR;
                    let remote_irr = if old_low == new_low {
                        *entry & REDIRECTION_ENTRY_REMOTE_IRR
                    } else {
                        state.pending_level[index] = false;
                        0
                    };
                    *entry = (*entry & !0xffff_ffff) | new_low | remote_irr;
                    if *entry & REDIRECTION_ENTRY_MASKED != 0 {
                        state.pending_level[index] = false;
                    }
                } else {
                    *entry = (*entry & 0xffff_ffff) | ((value as u64) << 32);
                }
                Ok(())
            }
            reg => {
                debug!("vIOAPIC write to unsupported register {reg:#x} = {value:#x}");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_iowin(state: &mut IoApicState, value: u32) {
        EmulatedIoApic::write_selected_register(state, value).unwrap();
    }

    fn select(state: &mut IoApicState, reg: u32) {
        state.selector = reg;
    }

    fn program_level_gsi(state: &mut IoApicState, gsi: usize, vector: u8) {
        select(state, IOREDTBL_BASE + (gsi as u32) * 2);
        write_iowin(state, REDIRECTION_ENTRY_TRIGGER_MODE as u32 | vector as u32);
        select(state, IOREDTBL_BASE + (gsi as u32) * 2 + 1);
        write_iowin(state, 0);
    }

    #[test]
    fn eoi_reports_gsi_and_deferred_level_interrupt() {
        let mut state = IoApicState::new();
        program_level_gsi(&mut state, 18, 0x51);

        assert_eq!(
            state.interrupt_for_entry(18),
            Some(IoApicInterrupt {
                vector: 0x51,
                level_triggered: true,
            })
        );
        assert_eq!(state.interrupt_for_entry(18), None);

        assert_eq!(
            state.end_of_interrupt(0x51),
            Some(IoApicEoi {
                gsi: 18,
                pending: Some(IoApicInterrupt {
                    vector: 0x51,
                    level_triggered: true,
                }),
            })
        );
    }
}

impl Default for EmulatedIoApic {
    fn default() -> Self {
        Self::new_default()
    }
}

impl EmulatedIoApic {
    /// Returns the IO APIC MMIO range.
    pub fn address_range(&self) -> X86GuestPhysAddrRange {
        X86GuestPhysAddrRange::new(
            self.base,
            X86GuestPhysAddr::from_usize(self.base.as_usize() + self.size),
        )
    }

    /// Handles an IO APIC MMIO read.
    pub fn handle_read(
        &self,
        addr: X86GuestPhysAddr,
        width: X86AccessWidth,
    ) -> X86VlapicResult<usize> {
        if !matches!(width, X86AccessWidth::Dword | X86AccessWidth::Qword) {
            return Err(X86VlapicError::Unsupported);
        }

        let offset = self.offset(addr);
        let state = self.state.lock();
        match offset {
            IOREGSEL => Ok(state.selector as usize),
            IOWIN => Ok(Self::read_selected_register(&state)? as usize),
            _ => {
                debug!("vIOAPIC read from unsupported offset {offset:#x}");
                Ok(0)
            }
        }
    }

    /// Handles an IO APIC MMIO write.
    pub fn handle_write(
        &self,
        addr: X86GuestPhysAddr,
        width: X86AccessWidth,
        val: usize,
    ) -> X86VlapicResult {
        if !matches!(width, X86AccessWidth::Dword | X86AccessWidth::Qword) {
            return Err(X86VlapicError::Unsupported);
        }

        let offset = self.offset(addr);
        let mut state = self.state.lock();
        match offset {
            IOREGSEL => {
                state.selector = val as u32;
                Ok(())
            }
            IOWIN => Self::write_selected_register(&mut state, val as u32),
            _ => {
                debug!("vIOAPIC write to unsupported offset {offset:#x} = {val:#x}");
                Ok(())
            }
        }
    }
}

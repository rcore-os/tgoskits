use super::super::csrs::{traps, RiscvCsrTrait, CSR};

pub const MAX_CPUS: usize = 8;
/// Number of contexts for the PLIC. Value is twice the max number of harts because each hart will
/// have one M-mode context and one S-mode context.
pub const MAX_CONTEXTS: usize = 2 * MAX_CPUS;

pub struct PlicState {
    base: usize,
    source_priority: [u32; 512],
    pending: [u32; 16],
    enable: [[u32; 32]; MAX_CONTEXTS],
    thresholds: [u32; MAX_CONTEXTS],
    pub claim_complete: [u32; MAX_CONTEXTS],
}

impl PlicState {
    pub fn new(base: usize) -> Self {
        Self {
            base,
            source_priority: [0; 512],
            pending: [0; 16],
            enable: [[0; 32]; MAX_CONTEXTS],
            thresholds: [0; MAX_CONTEXTS],
            claim_complete: [0; MAX_CONTEXTS],
        }
    }

    pub fn base(&self) -> usize {
        self.base
    }

    pub fn read_u32(&mut self, addr: usize) -> u32 {
        let offset = addr.wrapping_sub(self.base);
        if (0x20_0000..0x20_0000 + 0x1000 * MAX_CONTEXTS).contains(&offset) {
            // threshold/claim/complete
            let hart = (offset - 0x200000) / 0x1000;
            let index = ((offset - 0x200000) & 0xfff) >> 2;
            if index == 1 {
                // debug!("PLIC read@{:#x} -> {:#x}", addr, self.claim_complete[hart]);
                return self.claim_complete[hart];
            }
            todo!()
        }
        todo!()
    }

    pub fn write_u32(&mut self, addr: usize, val: u32) {
        // debug!("PLIC write@{:#x} -> {:#x}", addr, val);
        let offset = addr.wrapping_sub(self.base);
        // threshold/claim/complete
        if (0x200000..0x200000 + 0x1000 * MAX_CONTEXTS).contains(&offset) {
            let hart = (offset - 0x200000) / 0x1000;
            let index = ((offset - 0x200000) & 0xfff) >> 2;
            if index == 0 {
                // threshold
                self.thresholds[hart] = val;
                unsafe {
                    core::ptr::write_volatile(addr as *mut u32, val);
                }
            } else if index == 1 {
                // claim
                unsafe {
                    core::ptr::write_volatile(addr as *mut u32, val);
                }
                self.claim_complete[hart] = 0;
                // Send Interrupt to the hart
                CSR.hvip
                    .read_and_clear_bits(traps::interrupt::VIRTUAL_SUPERVISOR_EXTERNAL);
            }
        } else {
            todo!()
        }
    }
}

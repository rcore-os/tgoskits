use super::*;

/// Restricted non-blocking TX view used only for emergency output.
pub struct Pl011EmergencyTx {
    pub(super) base: Reg,
}

pub(super) struct Pl011EmergencyIrqMask<'a> {
    emergency: &'a Pl011EmergencyTx,
    enabled: u32,
}

impl Drop for Pl011EmergencyIrqMask<'_> {
    fn drop(&mut self) {
        self.emergency.registers().uartimsc.set(self.enabled);
    }
}

impl Pl011EmergencyTx {
    fn registers(&self) -> &Pl011Registers {
        // SAFETY: `base` points at the mapped PL011 register block. This view
        // exposes only the TX FIFO readiness and data registers.
        unsafe { &*self.base.0.as_ptr() }
    }

    pub(super) fn mask_interrupts(&self) -> Pl011EmergencyIrqMask<'_> {
        let enabled = self.registers().uartimsc.get();
        self.registers().uartimsc.set(0);
        // Flush a posted MMIO write before the emergency path touches TX.
        let _masked = self.registers().uartimsc.get();
        Pl011EmergencyIrqMask {
            emergency: self,
            enabled,
        }
    }
}

impl UartEmergencyTx for Pl011EmergencyTx {
    unsafe fn try_write_unlocked(&self, bytes: &[u8]) -> usize {
        let _irq_mask = self.mask_interrupts();
        let mut written = 0;
        for &byte in bytes.iter().take(EMERGENCY_TX_BUDGET) {
            if self.registers().uartfr.is_set(UARTFR::TXFF) {
                break;
            }
            self.registers().uartdr.set(byte as u32);
            written += 1;
        }
        written
    }
}

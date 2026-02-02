use crate::interrupt::VgicInt;
use crate::registers::GicRegister;
use crate::vgicd::Vgicd;
use axerrno::AxResult;
use axvisor_api::vmm::current_vcpu_id;
use spin::Mutex;

/// Virtual Generic Interrupt Controller.
///
/// Manages virtual interrupt distribution for guest VMs.
pub struct Vgic {
    vgicd: Mutex<Vgicd>,
}

impl Default for Vgic {
    fn default() -> Self {
        Self::new()
    }
}

impl Vgic {
    /// Creates a new VGIC instance.
    pub fn new() -> Vgic {
        Vgic {
            vgicd: Mutex::new(Vgicd::new()),
        }
    }
    pub(crate) fn handle_read8(&self, addr: usize) -> AxResult<usize> {
        let value = self.handle_read32(addr)?;
        Ok((value >> (8 * (addr & 0x3))) & 0xff)
    }

    pub(crate) fn handle_read16(&self, addr: usize) -> AxResult<usize> {
        let value = self.handle_read32(addr)?;
        Ok((value >> (8 * (addr & 0x3))) & 0xffff)
    }

    /// Handles 32-bit read access to VGIC registers.
    pub fn handle_read32(&self, addr: usize) -> AxResult<usize> {
        match GicRegister::from_addr(addr as u32) {
            Some(reg) => match reg {
                GicRegister::GicdCtlr => Ok(self.vgicd.lock().ctrlr as usize),
                GicRegister::GicdTyper => Ok(self.vgicd.lock().typer as usize),
                GicRegister::GicdIidr => Ok(self.vgicd.lock().iidr as usize),
                // // GicRegister::GicdStatusr => self.read_statusr(),
                // // GicRegister::GicdIgroupr(idx) => self.read_igroupr(idx),
                GicRegister::GicdIsenabler(idx) => Ok(self.vgicd.lock().vgicd_isenabler_read(idx)),
                // GicRegister::GicdIcenabler(idx) => self.read_icenabler(idx),
                // GicRegister::GicdIspendr(idx) => self.read_ispendr(idx),
                _ => {
                    // error!("Read register address: {:#x}", addr);
                    Ok(0)
                }
            },
            None => {
                //error!("Invalid read register address: {addr:#x}");
                Ok(0)
            }
        }
    }

    /// Handles 8-bit write access to VGIC registers.
    pub fn handle_write8(&self, addr: usize, value: usize) {
        self.handle_write32(addr, value);
    }

    /// Handles 16-bit write access to VGIC registers.
    pub fn handle_write16(&self, addr: usize, value: usize) {
        self.handle_write32(addr, value);
    }

    /// Handles 32-bit write access to VGIC registers.
    pub fn handle_write32(&self, addr: usize, value: usize) {
        let _vcpu_id = current_vcpu_id();
        if let Some(reg) = GicRegister::from_addr(addr as u32) {
            match reg {
                GicRegister::GicdCtlr => self.vgicd.lock().vgicd_ctrlr_write(value),
                // GicRegister::GicdIsenabler(idx) => self.write_isenabler(idx, value),
                GicRegister::GicdIsenabler(idx) => {
                    self.vgicd.lock().vgicd_isenabler_write(idx, value)
                }
                _ => {
                    //error!("Write register address: {:#x}", addr);
                }
            }
        }
    }

    // Removed, interrupt injection in arm_vcpu
    // pub fn inject_irq(&self, irq: u32) {
    //     self.vgicd.lock().inject_irq(irq);
    // }

    /// Fetches interrupt information for the given IRQ number.
    pub fn fetch_irq(&self, irq: u32) -> VgicInt {
        self.vgicd.lock().fetch_irq(irq)
    }

    /// Placeholder method for unused operations.
    pub fn nothing(&self, _value: u32) {}
}

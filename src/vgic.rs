use crate::interrupt::VgicInt;
use crate::registers::GicRegister;
use crate::vgicd::Vgicd;
use axerrno::AxResult;
use axvisor_api::vmm::{current_vcpu_id, current_vm_vcpu_num};
use spin::Mutex;

// 实现 Vgic
pub struct Vgic {
    vgicd: Mutex<Vgicd>,
}

impl Vgic {
    pub fn new() -> Vgic {
        Vgic {
            vgicd: Mutex::new(Vgicd::new()),
        }
    }
    pub(crate) fn handle_read8(&self, addr: usize) -> AxResult<usize> {
        let value = self.handle_read32(addr)?;
        return Ok((value >> (8 * (addr & 0x3))) & 0xff);
    }

    pub(crate) fn handle_read16(&self, addr: usize) -> AxResult<usize> {
        let value = self.handle_read32(addr)?;
        return Ok((value >> (8 * (addr & 0x3))) & 0xffff);
    }

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

    pub fn handle_write8(&self, addr: usize, value: usize) {
        self.handle_write32(addr, value);
    }

    pub fn handle_write16(&self, addr: usize, value: usize) {
        self.handle_write32(addr, value);
    }

    pub fn handle_write32(&self, addr: usize, value: usize) {
        let vcpu_id = current_vcpu_id();
        match GicRegister::from_addr(addr as u32) {
            Some(reg) => {
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
            None => {} //error!("Invalid write register address: {addr:#x}"),
        }
    }

    // Removed, interrupt injection in arm_vcpu
    // pub fn inject_irq(&self, irq: u32) {
    //     self.vgicd.lock().inject_irq(irq);
    // }

    pub fn fetch_irq(&self, irq: u32) -> VgicInt {
        self.vgicd.lock().fetch_irq(irq)
    }

    pub fn nothing(&self, _value: u32) {}
}

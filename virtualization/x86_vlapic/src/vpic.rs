use crate::{lock::SpinMutex as Mutex, *};

const MASTER_COMMAND: u16 = 0x20;
const MASTER_DATA: u16 = 0x21;
const SLAVE_COMMAND: u16 = 0xa0;
const SLAVE_DATA: u16 = 0xa1;
const CASCADE_IRQ: u8 = 2;

#[derive(Clone, Copy, Debug)]
struct PicChip {
    vector_base: u8,
    mask: u8,
    request: u8,
    in_service: u8,
    init_step: u8,
    needs_icw4: bool,
    single: bool,
    auto_eoi: bool,
    read_isr: bool,
}

impl PicChip {
    const fn new(vector_base: u8) -> Self {
        Self {
            vector_base,
            mask: u8::MAX,
            request: 0,
            in_service: 0,
            init_step: 0,
            needs_icw4: false,
            single: false,
            auto_eoi: false,
            read_isr: false,
        }
    }

    fn command(&mut self, value: u8) {
        if value & 0x10 != 0 {
            self.request = 0;
            self.in_service = 0;
            self.init_step = 1;
            self.needs_icw4 = value & 0x01 != 0;
            self.single = value & 0x02 != 0;
            self.auto_eoi = false;
            self.read_isr = false;
            return;
        }

        if value & 0x18 == 0x08 {
            self.read_isr = value & 0x02 != 0;
            return;
        }

        if value & 0x20 != 0 {
            if value & 0x40 != 0 {
                self.in_service &= !(1 << (value & 0x07));
            } else if let Some(irq) = highest_priority(self.in_service) {
                self.in_service &= !(1 << irq);
            }
        }
    }

    fn data(&mut self, value: u8) {
        match self.init_step {
            1 => {
                self.vector_base = value & 0xf8;
                self.init_step = if self.single {
                    u8::from(self.needs_icw4) * 3
                } else {
                    2
                };
            }
            2 => {
                self.init_step = if self.needs_icw4 { 3 } else { 0 };
            }
            3 => {
                self.auto_eoi = value & 0x02 != 0;
                self.init_step = 0;
            }
            _ => self.mask = value,
        }
    }

    fn read_command(&self) -> u8 {
        if self.read_isr {
            self.in_service
        } else {
            self.request
        }
    }

    fn pulse(&mut self, irq: u8) {
        self.request |= 1 << irq;
    }

    fn pending_irq(&self) -> Option<u8> {
        let irq = highest_priority(self.request & !self.mask)?;
        let Some(in_service) = highest_priority(self.in_service) else {
            return Some(irq);
        };
        (irq < in_service).then_some(irq)
    }

    fn acknowledge(&mut self, irq: u8) -> u8 {
        self.request &= !(1 << irq);
        if !self.auto_eoi {
            self.in_service |= 1 << irq;
        }
        self.vector_base.wrapping_add(irq)
    }
}

fn highest_priority(bits: u8) -> Option<u8> {
    (bits != 0).then(|| bits.trailing_zeros() as u8)
}

#[derive(Clone, Copy, Debug)]
struct PicState {
    master: PicChip,
    slave: PicChip,
}

impl PicState {
    const fn new() -> Self {
        Self {
            master: PicChip::new(0x08),
            slave: PicChip::new(0x70),
        }
    }

    fn pulse_irq(&mut self, irq: u8) -> Option<u8> {
        if irq < 8 {
            self.master.pulse(irq);
        } else if irq < 16 {
            self.slave.pulse(irq - 8);
            self.master.pulse(CASCADE_IRQ);
        } else {
            return None;
        }
        self.next_interrupt()
    }

    fn next_interrupt(&mut self) -> Option<u8> {
        let master_irq = self.master.pending_irq()?;
        if master_irq != CASCADE_IRQ {
            return Some(self.master.acknowledge(master_irq));
        }

        let Some(slave_irq) = self.slave.pending_irq() else {
            return Some(self.master.acknowledge(master_irq));
        };
        self.master.acknowledge(master_irq);
        Some(self.slave.acknowledge(slave_irq))
    }
}

/// Guest-owned pair of legacy 8259-compatible interrupt controllers.
pub struct EmulatedPic {
    state: Mutex<PicState>,
}

impl EmulatedPic {
    /// Creates the reset-compatible master and slave PIC state.
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(PicState::new()),
        }
    }

    /// Returns the two standard command/data port ranges.
    pub const fn port_ranges() -> [X86PortRange; 2] {
        [
            X86PortRange::new(X86Port::new(MASTER_COMMAND), X86Port::new(MASTER_DATA)),
            X86PortRange::new(X86Port::new(SLAVE_COMMAND), X86Port::new(SLAVE_DATA)),
        ]
    }

    /// Latches an edge on one legacy IRQ and returns an immediately deliverable vector.
    pub fn pulse_irq(&self, irq: u8) -> Option<u8> {
        self.state.lock().pulse_irq(irq)
    }

    /// Handles one byte-wide PIC port read.
    pub fn handle_read(&self, port: X86Port, width: X86AccessWidth) -> X86VlapicResult<usize> {
        if width != X86AccessWidth::Byte {
            return Err(X86VlapicError::Unsupported);
        }
        let state = self.state.lock();
        let value = match port.number() {
            MASTER_COMMAND => state.master.read_command(),
            MASTER_DATA => state.master.mask,
            SLAVE_COMMAND => state.slave.read_command(),
            SLAVE_DATA => state.slave.mask,
            _ => return Err(X86VlapicError::Unsupported),
        };
        Ok(value as usize)
    }

    /// Handles one byte-wide PIC port write.
    pub fn handle_write(
        &self,
        port: X86Port,
        width: X86AccessWidth,
        value: usize,
    ) -> X86VlapicResult {
        if width != X86AccessWidth::Byte {
            return Err(X86VlapicError::Unsupported);
        }
        let mut state = self.state.lock();
        match port.number() {
            MASTER_COMMAND => state.master.command(value as u8),
            MASTER_DATA => state.master.data(value as u8),
            SLAVE_COMMAND => state.slave.command(value as u8),
            SLAVE_DATA => state.slave.data(value as u8),
            _ => return Err(X86VlapicError::Unsupported),
        }
        Ok(())
    }
}

impl Default for EmulatedPic {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(pic: &EmulatedPic, port: u16, value: u8) {
        pic.handle_write(X86Port::new(port), X86AccessWidth::Byte, value as usize)
            .unwrap();
    }

    #[test]
    fn firmware_can_reprogram_and_service_pit_irq0() {
        let pic = EmulatedPic::new();
        write(&pic, MASTER_COMMAND, 0x11);
        write(&pic, MASTER_DATA, 0x68);
        write(&pic, MASTER_DATA, 0x04);
        write(&pic, MASTER_DATA, 0x01);
        write(&pic, MASTER_DATA, 0xfe);

        assert_eq!(pic.pulse_irq(0), Some(0x68));
        assert_eq!(pic.pulse_irq(0), None);
        write(&pic, MASTER_COMMAND, 0x20);
        assert_eq!(pic.pulse_irq(0), Some(0x68));
    }
}

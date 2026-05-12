use core::ptr::NonNull;

pub const CTRL: usize = 0x0000;
pub const ICR: usize = 0x00C0;
pub const IMS: usize = 0x00D0;
pub const IMC: usize = 0x00D8;
pub const RCTL: usize = 0x0100;
pub const TCTL: usize = 0x0400;
pub const TIPG: usize = 0x0410;
pub const RDBAL: usize = 0x2800;
pub const RDBAH: usize = 0x2804;
pub const RDLEN: usize = 0x2808;
pub const RDH: usize = 0x2810;
pub const RDT: usize = 0x2818;
pub const TDBAL: usize = 0x3800;
pub const TDBAH: usize = 0x3804;
pub const TDLEN: usize = 0x3808;
pub const TDH: usize = 0x3810;
pub const TDT: usize = 0x3818;
pub const RAL0: usize = 0x5400;
pub const RAH0: usize = 0x5404;

#[derive(Clone, Copy)]
pub struct Regs {
    base: NonNull<u8>,
}

unsafe impl Send for Regs {}
unsafe impl Sync for Regs {}

impl Regs {
    pub fn new(base: NonNull<u8>) -> Self {
        Self { base }
    }

    #[inline]
    pub fn read(&self, offset: usize) -> u32 {
        unsafe { self.base.as_ptr().add(offset).cast::<u32>().read_volatile() }
    }

    #[inline]
    pub fn write(&self, offset: usize, value: u32) {
        unsafe {
            self.base
                .as_ptr()
                .add(offset)
                .cast::<u32>()
                .write_volatile(value)
        }
    }

    pub fn reset(&self) {
        self.write(CTRL, self.read(CTRL) | (1 << 26));
        for _ in 0..20000 {
            if self.read(CTRL) & (1 << 26) == 0 {
                break;
            }
            core::hint::spin_loop();
        }
    }

    pub fn disable_all_irq(&self) {
        self.write(IMC, u32::MAX);
        let _ = self.read(ICR);
    }

    pub fn enable_default_irq(&self) {
        // TXDW + LSC + RXT0
        self.write(IMS, (1 << 0) | (1 << 2) | (1 << 7));
    }

    pub fn mac_addr(&self) -> [u8; 6] {
        let low = self.read(RAL0);
        let high = self.read(RAH0);
        [
            (low & 0xff) as u8,
            ((low >> 8) & 0xff) as u8,
            ((low >> 16) & 0xff) as u8,
            ((low >> 24) & 0xff) as u8,
            (high & 0xff) as u8,
            ((high >> 8) & 0xff) as u8,
        ]
    }
}

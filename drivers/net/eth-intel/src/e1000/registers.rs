use core::{mem::size_of, ptr::NonNull};

pub const CTRL: usize = 0x0000;
pub const ICR: usize = 0x00c0;
pub const IMS: usize = 0x00d0;
pub const IMC: usize = 0x00d8;
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
pub const E1000_REGS_SIZE: usize = RAH0 + size_of::<u32>();

const CTRL_SLU: u32 = 1 << 6;
const CTRL_RESET: u32 = 1 << 26;
const DEFAULT_IRQ_MASK: u32 = (1 << 0) | (1 << 2) | (1 << 7);
const RCTL_ENABLE: u32 = 1 << 1;
const RCTL_CONFIG: u32 = (1 << 15) | (1 << 26);
const TCTL_CONFIG: u32 = (1 << 1) | (1 << 3) | (0x10 << 4) | (0x40 << 12);
const TIPG_CONFIG: u32 = 10 | (8 << 10) | (6 << 20);

/// Move-only raw register capability used to construct disjoint role ports.
struct RegisterPort {
    base: NonNull<u8>,
}

impl RegisterPort {
    const fn new(base: NonNull<u8>) -> Self {
        Self { base }
    }

    fn read(&self, offset: usize) -> u32 {
        unsafe {
            // SAFETY: discovery validates the owning mapping through
            // `E1000_REGS_SIZE`; role ports never outlive that mapping lease.
            self.base.as_ptr().add(offset).cast::<u32>().read_volatile()
        }
    }

    fn write(&self, offset: usize, value: u32) {
        unsafe {
            // SAFETY: the role APIs below partition register ownership and
            // every offset is inside the validated E1000 BAR mapping.
            self.base
                .as_ptr()
                .add(offset)
                .cast::<u32>()
                .write_volatile(value);
        }
    }

    fn into_raw(self) -> NonNull<u8> {
        self.base
    }
}

// SAFETY: a port may move to its final maintenance owner or IRQ action. It is
// deliberately not `Sync`; concurrency is expressed only by the role split.
unsafe impl Send for RegisterPort {}

/// Discovery-only capability. Constructing it performs no device access.
pub struct E1000DiscoveryRegs {
    port: RegisterPort,
}

impl E1000DiscoveryRegs {
    pub const fn new(base: NonNull<u8>) -> Self {
        Self {
            port: RegisterPort::new(base),
        }
    }

    /// Creates the destructive IRQ port and the temporally exclusive owner
    /// initialization port. This is the only raw-address duplication point.
    pub fn split_for_irq(self) -> (E1000OwnerInitRegs, E1000IrqPort) {
        let base = self.port.into_raw();
        (
            E1000OwnerInitRegs {
                port: RegisterPort::new(base),
            },
            E1000IrqPort {
                port: RegisterPort::new(base),
            },
        )
    }
}

/// Register capability used only by the CPU-pinned initialization FSM.
pub struct E1000OwnerInitRegs {
    port: RegisterPort,
}

impl E1000OwnerInitRegs {
    pub fn mask_interrupts(&self) {
        self.port.write(IMC, u32::MAX);
    }

    pub fn begin_reset(&self) {
        self.port.write(CTRL, self.port.read(CTRL) | CTRL_RESET);
    }

    pub fn reset_pending(&self) -> bool {
        self.port.read(CTRL) & CTRL_RESET != 0
    }

    pub fn set_link_up(&self) {
        self.port.write(CTRL, self.port.read(CTRL) | CTRL_SLU);
    }

    pub fn mac_address(&self) -> [u8; 6] {
        let low = self.port.read(RAL0);
        let high = self.port.read(RAH0);
        [
            (low & 0xff) as u8,
            ((low >> 8) & 0xff) as u8,
            ((low >> 16) & 0xff) as u8,
            ((low >> 24) & 0xff) as u8,
            (high & 0xff) as u8,
            ((high >> 8) & 0xff) as u8,
        ]
    }

    pub fn program_queues(&self, tx_base: u64, tx_len: u32, rx_base: u64, rx_len: u32) {
        self.port.write(TDBAL, tx_base as u32);
        self.port.write(TDBAH, (tx_base >> 32) as u32);
        self.port.write(TDLEN, tx_len);
        self.port.write(TDH, 0);
        self.port.write(TDT, 0);

        self.port.write(RDBAL, rx_base as u32);
        self.port.write(RDBAH, (rx_base >> 32) as u32);
        self.port.write(RDLEN, rx_len);
        self.port.write(RDH, 0);
        self.port.write(RDT, 0);

        self.port.write(TCTL, TCTL_CONFIG);
        self.port.write(TIPG, TIPG_CONFIG);
        // RX stays disabled until the owner has published at least one valid
        // descriptor. This prevents DMA through zeroed discovery storage.
        self.port.write(RCTL, RCTL_CONFIG);
    }

    pub fn into_runtime_ports(self) -> (E1000OwnerRegs, E1000TxRegs, E1000RxRegs) {
        let base = self.port.into_raw();
        (
            E1000OwnerRegs {
                port: RegisterPort::new(base),
            },
            E1000TxRegs {
                port: RegisterPort::new(base),
            },
            E1000RxRegs {
                port: RegisterPort::new(base),
            },
        )
    }
}

/// Non-destructive controller port retained by the maintenance owner.
pub struct E1000OwnerRegs {
    port: RegisterPort,
}

impl E1000OwnerRegs {
    pub fn mask_interrupts(&self) {
        self.port.write(IMC, u32::MAX);
    }

    pub fn enable_default_interrupts(&self) {
        self.port.write(IMS, DEFAULT_IRQ_MASK);
    }
}

/// TX queue register port. It cannot observe destructive IRQ status.
pub struct E1000TxRegs {
    port: RegisterPort,
}

impl E1000TxRegs {
    pub fn head(&self) -> usize {
        self.port.read(TDH) as usize
    }

    pub fn publish_tail(&self, tail: usize) {
        self.port.write(TDT, tail as u32);
    }
}

/// RX queue register port. It cannot observe destructive IRQ status.
pub struct E1000RxRegs {
    port: RegisterPort,
}

impl E1000RxRegs {
    pub fn head(&self) -> usize {
        self.port.read(RDH) as usize
    }

    pub fn publish_tail(&self, tail: usize) {
        self.port.write(RDT, tail as u32);
    }

    pub fn enable_receiver(&self) {
        self.port.write(RCTL, RCTL_CONFIG | RCTL_ENABLE);
    }
}

/// Destructive interrupt status and exact-source containment port.
pub struct E1000IrqPort {
    port: RegisterPort,
}

impl E1000IrqPort {
    pub fn capture_status(&self) -> Option<u32> {
        let status = self.port.read(ICR);
        (status != 0 && status != u32::MAX).then_some(status)
    }

    pub fn mask_interrupts(&self) {
        self.port.write(IMC, u32::MAX);
    }
}

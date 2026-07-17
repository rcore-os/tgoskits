use alloc::sync::Arc;

use mmio_api::Mmio;

use crate::AhciError;

pub(crate) const MAX_PORTS: usize = 32;
pub(crate) const PORT_BASE: usize = 0x100;
pub(crate) const PORT_STRIDE: usize = 0x80;
pub(crate) const MMIO_REQUIRED_SIZE: usize = PORT_BASE + MAX_PORTS * PORT_STRIDE;

pub(crate) const HOST_CAP: usize = 0x00;
pub(crate) const HOST_GHC: usize = 0x04;
pub(crate) const HOST_IS: usize = 0x08;
pub(crate) const HOST_PI: usize = 0x0c;
pub(crate) const HOST_CAP2: usize = 0x24;
pub(crate) const HOST_BOHC: usize = 0x28;

pub(crate) const GHC_HR: u32 = 1 << 0;
pub(crate) const GHC_IE: u32 = 1 << 1;
pub(crate) const GHC_AE: u32 = 1 << 31;
pub(crate) const CAP_S64A: u32 = 1 << 31;
pub(crate) const CAP2_BOH: u32 = 1 << 0;
pub(crate) const BOHC_BOS: u32 = 1 << 0;
pub(crate) const BOHC_OOS: u32 = 1 << 1;
pub(crate) const BOHC_BB: u32 = 1 << 4;

pub(crate) const PX_CLB: usize = 0x00;
pub(crate) const PX_CLBU: usize = 0x04;
pub(crate) const PX_FB: usize = 0x08;
pub(crate) const PX_FBU: usize = 0x0c;
pub(crate) const PX_IS: usize = 0x10;
pub(crate) const PX_IE: usize = 0x14;
pub(crate) const PX_CMD: usize = 0x18;
pub(crate) const PX_TFD: usize = 0x20;
pub(crate) const PX_SSTS: usize = 0x28;
pub(crate) const PX_SCTL: usize = 0x2c;
pub(crate) const PX_SERR: usize = 0x30;
pub(crate) const PX_SACT: usize = 0x34;
pub(crate) const PX_CI: usize = 0x38;

pub(crate) const CMD_ST: u32 = 1 << 0;
pub(crate) const CMD_SUD: u32 = 1 << 1;
pub(crate) const CMD_POD: u32 = 1 << 2;
pub(crate) const CMD_FRE: u32 = 1 << 4;
pub(crate) const CMD_FR: u32 = 1 << 14;
pub(crate) const CMD_CR: u32 = 1 << 15;
pub(crate) const CMD_ICC_ACTIVE: u32 = 1 << 28;

pub(crate) const TFD_ERR: u32 = 1 << 0;
pub(crate) const TFD_DRQ: u32 = 1 << 3;
pub(crate) const TFD_BSY: u32 = 1 << 7;
pub(crate) const TFD_NOT_READY: u32 = TFD_ERR | TFD_DRQ | TFD_BSY;

pub(crate) const SCTL_DET_MASK: u32 = 0xf;
pub(crate) const SCTL_DET_NONE: u32 = 0;
pub(crate) const SCTL_DET_INIT: u32 = 1;

pub(crate) const IRQ_D2H_REG_FIS: u32 = 1 << 0;
pub(crate) const IRQ_PIO_SETUP_FIS: u32 = 1 << 1;
pub(crate) const IRQ_DMA_SETUP_FIS: u32 = 1 << 2;
pub(crate) const IRQ_SET_DEVICE_BITS_FIS: u32 = 1 << 3;
pub(crate) const IRQ_UNKNOWN_FIS: u32 = 1 << 4;
pub(crate) const IRQ_DESCRIPTOR_PROCESSED: u32 = 1 << 5;
pub(crate) const IRQ_CONNECT_CHANGE: u32 = 1 << 6;
pub(crate) const IRQ_PHY_READY_CHANGE: u32 = 1 << 22;
pub(crate) const IRQ_BAD_PORT_MULTIPLIER: u32 = 1 << 23;
pub(crate) const IRQ_OVERFLOW: u32 = 1 << 24;
pub(crate) const IRQ_INTERFACE_NONFATAL: u32 = 1 << 26;
pub(crate) const IRQ_INTERFACE_FATAL: u32 = 1 << 27;
pub(crate) const IRQ_HOST_BUS_DATA_ERROR: u32 = 1 << 28;
pub(crate) const IRQ_HOST_BUS_FATAL_ERROR: u32 = 1 << 29;
pub(crate) const IRQ_TASK_FILE_ERROR: u32 = 1 << 30;
pub(crate) const IRQ_COLD_PRESENCE: u32 = 1 << 31;

pub(crate) const IRQ_COMPLETION: u32 =
    IRQ_DESCRIPTOR_PROCESSED | IRQ_SET_DEVICE_BITS_FIS | IRQ_PIO_SETUP_FIS | IRQ_D2H_REG_FIS;
pub(crate) const IRQ_FREEZE: u32 = IRQ_HOST_BUS_FATAL_ERROR
    | IRQ_INTERFACE_FATAL
    | IRQ_CONNECT_CHANGE
    | IRQ_PHY_READY_CHANGE
    | IRQ_UNKNOWN_FIS
    | IRQ_BAD_PORT_MULTIPLIER
    | IRQ_COLD_PRESENCE;
pub(crate) const IRQ_ERROR: u32 =
    IRQ_FREEZE | IRQ_TASK_FILE_ERROR | IRQ_HOST_BUS_DATA_ERROR | IRQ_OVERFLOW;
pub(crate) const DEFAULT_PORT_IRQ_MASK: u32 = IRQ_ERROR
    | IRQ_DESCRIPTOR_PROCESSED
    | IRQ_SET_DEVICE_BITS_FIS
    | IRQ_DMA_SETUP_FIS
    | IRQ_PIO_SETUP_FIS
    | IRQ_D2H_REG_FIS
    | IRQ_OVERFLOW
    | IRQ_INTERFACE_NONFATAL
    | IRQ_COLD_PRESENCE;

pub(crate) trait RegisterIo: Send + Sync {
    fn read32(&self, offset: usize) -> u32;
    fn write32(&self, offset: usize, value: u32);
}

pub(crate) struct MappedRegisters {
    mapping: Mmio,
}

impl MappedRegisters {
    pub(crate) fn new(mapping: Mmio) -> Result<Self, AhciError> {
        if mapping.size() < MMIO_REQUIRED_SIZE {
            return Err(AhciError::MmioApertureTooSmall {
                required: MMIO_REQUIRED_SIZE,
                actual: mapping.size(),
            });
        }
        Ok(Self { mapping })
    }
}

impl RegisterIo for MappedRegisters {
    fn read32(&self, offset: usize) -> u32 {
        self.mapping.read(offset)
    }

    fn write32(&self, offset: usize, value: u32) {
        self.mapping.write(offset, value);
    }
}

pub(crate) type SharedRegisters = Arc<dyn RegisterIo>;

pub(crate) const fn port_offset(port: usize, register: usize) -> usize {
    PORT_BASE + port * PORT_STRIDE + register
}

pub(crate) fn read_port(registers: &dyn RegisterIo, port: usize, register: usize) -> u32 {
    registers.read32(port_offset(port, register))
}

pub(crate) fn write_port(registers: &dyn RegisterIo, port: usize, register: usize, value: u32) {
    registers.write32(port_offset(port, register), value);
}

#[cfg(test)]
pub(crate) mod tests_support {
    use alloc::{sync::Arc, vec, vec::Vec};
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::{RegisterIo, SharedRegisters};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) struct RegisterWrite {
        pub offset: usize,
        pub value: u32,
        pub sequence: usize,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) struct RegisterRead {
        pub offset: usize,
        pub sequence: usize,
    }

    pub(crate) struct FakeRegisters {
        words: Mutex<Vec<u32>>,
        reads: Mutex<Vec<RegisterRead>>,
        writes: Mutex<Vec<RegisterWrite>>,
        sequence: AtomicUsize,
    }

    impl FakeRegisters {
        pub(crate) fn new(byte_len: usize) -> Arc<Self> {
            Arc::new(Self {
                words: Mutex::new(vec![0; byte_len / 4]),
                reads: Mutex::new(Vec::new()),
                writes: Mutex::new(Vec::new()),
                sequence: AtomicUsize::new(0),
            })
        }

        pub(crate) fn shared(self: &Arc<Self>) -> SharedRegisters {
            Arc::clone(self) as SharedRegisters
        }

        pub(crate) fn set(&self, offset: usize, value: u32) {
            self.words.lock().unwrap()[offset / 4] = value;
        }

        pub(crate) fn writes(&self) -> Vec<RegisterWrite> {
            self.writes.lock().unwrap().clone()
        }

        pub(crate) fn reads(&self) -> Vec<RegisterRead> {
            self.reads.lock().unwrap().clone()
        }

        pub(crate) fn clear_access_log(&self) {
            self.reads.lock().unwrap().clear();
            self.writes.lock().unwrap().clear();
        }
    }

    impl RegisterIo for FakeRegisters {
        fn read32(&self, offset: usize) -> u32 {
            let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
            self.reads
                .lock()
                .unwrap()
                .push(RegisterRead { offset, sequence });
            self.words.lock().unwrap()[offset / 4]
        }

        fn write32(&self, offset: usize, value: u32) {
            let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
            self.writes.lock().unwrap().push(RegisterWrite {
                offset,
                value,
                sequence,
            });
            let mut words = self.words.lock().unwrap();
            let current = &mut words[offset / 4];
            if offset == super::HOST_IS
                || (offset >= super::PORT_BASE
                    && (offset - super::PORT_BASE) % super::PORT_STRIDE == super::PX_IS)
                || (offset >= super::PORT_BASE
                    && (offset - super::PORT_BASE) % super::PORT_STRIDE == super::PX_SERR)
            {
                *current &= !value;
            } else {
                *current = value;
            }
        }
    }
}

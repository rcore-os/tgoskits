use core::{
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use xhci::accessor::Mapper;

use super::SlotId;

#[derive(Debug, Clone, Copy)]
pub struct MemMapper;
impl Mapper for MemMapper {
    unsafe fn map(&mut self, phys_start: usize, _bytes: usize) -> NonZeroUsize {
        unsafe { NonZeroUsize::new_unchecked(phys_start) }
    }
    fn unmap(&mut self, _virt_start: usize, _bytes: usize) {}
}

type Registers = xhci::Registers<MemMapper>;
// type RegistersExtList = xhci::extended_capabilities::List<MemMapper>;
// type SupportedProtocol = xhci::extended_capabilities::XhciSupportedProtocol<MemMapper>;
pub(crate) type XhciRegistersShared = alloc::sync::Arc<spin::RwLock<XhciRegisters>>;

pub(crate) struct XhciRegisters {
    pub mmio_base: usize,
    reg: Registers,
}

impl Clone for XhciRegisters {
    fn clone(&self) -> Self {
        Self {
            mmio_base: self.mmio_base,
            reg: self.new_reg(),
        }
    }
}

impl XhciRegisters {
    pub fn new(mmio_base: NonNull<u8>) -> Self {
        let mmio_base = mmio_base.as_ptr() as usize;
        let mapper = MemMapper {};
        let reg = unsafe { Registers::new(mmio_base, mapper) };
        Self { mmio_base, reg }
    }

    fn new_reg(&self) -> Registers {
        let mapper = MemMapper {};
        unsafe { Registers::new(self.mmio_base, mapper) }
    }

    pub fn disable_irq_guard(&mut self) -> DisableIrqGuard {
        let mut enable = true;
        self.operational.usbcmd.update_volatile(|r| {
            enable = r.interrupter_enable();
            r.clear_interrupter_enable();
        });
        DisableIrqGuard {
            reg: self.new_reg(),
            enable,
        }
    }
}

impl Deref for XhciRegisters {
    type Target = Registers;

    fn deref(&self) -> &Self::Target {
        &self.reg
    }
}

impl DerefMut for XhciRegisters {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.reg
    }
}

pub struct DisableIrqGuard {
    reg: Registers,
    enable: bool,
}
impl Drop for DisableIrqGuard {
    fn drop(&mut self) {
        if self.enable {
            self.reg.operational.usbcmd.update_volatile(|r| {
                r.set_interrupter_enable();
            });
        }
    }
}

pub struct SlotBell {
    slot_id: SlotId,
    reg: XhciRegisters,
}

impl SlotBell {
    pub fn new(slot_id: SlotId, reg: XhciRegisters) -> Self {
        Self { slot_id, reg }
    }

    pub fn ring(&mut self, bell: xhci::registers::doorbell::Register) {
        self.reg
            .doorbell
            .write_volatile_at(self.slot_id.as_usize(), bell);
    }
}

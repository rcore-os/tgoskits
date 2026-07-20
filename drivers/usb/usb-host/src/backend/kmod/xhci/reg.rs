use core::{
    mem::size_of,
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use xhci::{
    accessor::{Mapper, single},
    extended_capabilities::usb_legacy_support_capability::UsbLegacySupport,
    registers::{capability::CapabilityParameters1, operational::PortStatusAndControlRegister},
};

use super::SlotId;

#[derive(Debug, Clone, Copy)]
pub struct MemMapper;
impl Mapper for MemMapper {
    unsafe fn map(&mut self, phys_start: usize, _bytes: usize) -> NonZeroUsize {
        unsafe { NonZeroUsize::new_unchecked(phys_start) }
    }
    fn unmap(&mut self, _virt_start: usize, _bytes: usize) {}
}

pub(super) struct ExtendedCapabilities<M>
where
    M: Mapper + Clone,
{
    pub(super) count: usize,
    pub(super) usb_legacy_support: Option<UsbLegacySupport<M>>,
}

/// Scans xHCI extended capabilities using one 32-bit MMIO read per header.
///
/// RK3588's DWC3 wrapper can lock up on widened MMIO transactions. Keeping the volatile value a
/// `u32` prevents aggregate header reads from being lowered to 64-bit or 128-bit loads.
pub(super) fn scan_extended_capabilities<M>(
    mmio_base: usize,
    hccparams1: CapabilityParameters1,
    mapper: M,
) -> ExtendedCapabilities<M>
where
    M: Mapper + Clone,
{
    const USB_LEGACY_SUPPORT_ID: u8 = 1;
    const MAX_CAPABILITIES: usize = 256;

    let pointer_dwords = usize::from(hccparams1.xhci_extended_capabilities_pointer());
    if pointer_dwords == 0 {
        return ExtendedCapabilities {
            count: 0,
            usb_legacy_support: None,
        };
    }

    let mut address = mmio_base + pointer_dwords * size_of::<u32>();
    let mut count = 0;
    let mut usb_legacy_support = None;
    for _ in 0..MAX_CAPABILITIES {
        let header = {
            // SAFETY: The xHCI capability pointer and each next offset identify an aligned
            // 32-bit MMIO header. The accessor is dropped before any capability accessor is made.
            let header = unsafe { single::ReadOnly::<u32, M>::new(address, mapper.clone()) };
            header.read_volatile()
        };
        count += 1;

        let capability_id = header as u8;
        if capability_id == USB_LEGACY_SUPPORT_ID && usb_legacy_support.is_none() {
            // SAFETY: The capability ID in the just-read header identifies USB Legacy Support,
            // and the temporary header accessor has already been dropped.
            usb_legacy_support = Some(unsafe { UsbLegacySupport::new(address, mapper.clone()) });
        }

        let next_dwords = usize::from((header >> 8) as u8);
        if next_dwords == 0 {
            break;
        }
        address += next_dwords * size_of::<u32>();
    }

    ExtendedCapabilities {
        count,
        usb_legacy_support,
    }
}

/// Accesses only the 32-bit PORTSC register in each xHCI port register set.
///
/// This intentionally avoids volatile access to the aggregate 16-byte port register set, which a
/// compiler may lower to widened MMIO transactions that the RK3588 DWC3 wrapper cannot handle.
pub(super) struct PortStatusRegisters<M>
where
    M: Mapper + Clone,
{
    base: usize,
    len: usize,
    mapper: M,
}

impl<M> PortStatusRegisters<M>
where
    M: Mapper + Clone,
{
    /// # Safety
    ///
    /// `base` must point to `len` valid xHCI port register sets, and no other accessor may
    /// concurrently access those registers.
    pub unsafe fn new(base: usize, len: usize, mapper: M) -> Self {
        Self { base, len, mapper }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn read_volatile_at(&self, index: usize) -> PortStatusAndControlRegister {
        self.register(index).read_volatile()
    }

    pub fn update_volatile_at<F>(&mut self, index: usize, update: F)
    where
        F: FnOnce(&mut PortStatusAndControlRegister),
    {
        self.register(index).update_volatile(update);
    }

    fn register(&self, index: usize) -> single::ReadWrite<PortStatusAndControlRegister, M> {
        const PORT_REGISTER_SET_STRIDE: usize = 0x10;

        assert!(index < self.len, "port register index out of range");
        let address = self.base + index * PORT_REGISTER_SET_STRIDE;
        // SAFETY: `new` guarantees that every PORTSC address in this range is valid and uniquely
        // accessed through this wrapper. Each temporary accessor covers only the first 32-bit
        // register in a port register set.
        unsafe { single::ReadWrite::new(address, self.mapper.clone()) }
    }
}

type Registers = xhci::Registers<MemMapper>;
// type RegistersExtList = xhci::extended_capabilities::List<MemMapper>;
// type SupportedProtocol = xhci::extended_capabilities::XhciSupportedProtocol<MemMapper>;
pub(crate) type XhciRegistersShared = alloc::sync::Arc<ax_kspin::SpinRwLock<XhciRegisters>>;

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

    pub(super) fn port_status_registers(&self) -> PortStatusRegisters<MemMapper> {
        let len = self.port_register_set.len();
        let base =
            self.mmio_base + usize::from(self.capability.caplength.read_volatile().get()) + 0x400;
        // SAFETY: The xHCI capability registers provide the base and number of port register
        // sets. Callers use this view only for each set's PORTSC dword.
        unsafe { PortStatusRegisters::new(base, len, MemMapper) }
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

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{mem::size_of, num::NonZeroUsize};
    use std::{cell::RefCell, rc::Rc, vec::Vec};

    use super::*;

    #[test]
    fn xhci_extended_capability_scan_uses_dword_mmio_only() {
        const EXT_CAP_POINTER_DWORDS: u32 = 1;
        const USB_LEGACY_SUPPORT_ID: u32 = 1;
        const SUPPORTED_PROTOCOL_ID: u32 = 2;
        const NEXT_CAPABILITY_DWORDS: u32 = 4;

        let mut registers = [0_u32; 9];
        registers[EXT_CAP_POINTER_DWORDS as usize] =
            USB_LEGACY_SUPPORT_ID | (NEXT_CAPABILITY_DWORDS << 8);
        registers[(EXT_CAP_POINTER_DWORDS + NEXT_CAPABILITY_DWORDS) as usize] =
            SUPPORTED_PROTOCOL_ID;

        let mapped_widths = Rc::new(RefCell::new(Vec::new()));
        let mapper = RecordingMapper {
            mapped_widths: mapped_widths.clone(),
        };
        let hccparams1 = capability_parameters_with_pointer(EXT_CAP_POINTER_DWORDS as u16);

        let capabilities =
            scan_extended_capabilities(registers.as_mut_ptr() as usize, hccparams1, mapper);

        assert_eq!(capabilities.count, 2);
        assert!(capabilities.usb_legacy_support.is_some());
        assert!(
            mapped_widths
                .borrow()
                .iter()
                .all(|&bytes| bytes == size_of::<u32>()),
            "xHCI extended capability scan used non-dword MMIO accesses: {:?}",
            mapped_widths.borrow()
        );
    }

    #[test]
    fn xhci_port_status_accesses_use_dword_mmio_only() {
        let mut registers = [0_u32; 8];
        registers[4] = 1;
        let mapped_widths = Rc::new(RefCell::new(Vec::new()));
        let mapper = RecordingMapper {
            mapped_widths: mapped_widths.clone(),
        };
        let mut portsc =
            unsafe { PortStatusRegisters::new(registers.as_mut_ptr() as usize, 2, mapper) };

        portsc.update_volatile_at(0, |register| {
            register.set_port_power();
        });
        let second_port = portsc.read_volatile_at(1);

        assert_eq!(registers[0], 1 << 9);
        assert!(second_port.current_connect_status());
        assert!(
            mapped_widths
                .borrow()
                .iter()
                .all(|&bytes| bytes == size_of::<u32>()),
            "xHCI PORTSC used non-dword MMIO accesses: {:?}",
            mapped_widths.borrow()
        );
    }

    #[derive(Clone)]
    struct RecordingMapper {
        mapped_widths: Rc<RefCell<Vec<usize>>>,
    }

    impl Mapper for RecordingMapper {
        unsafe fn map(&mut self, phys_start: usize, bytes: usize) -> NonZeroUsize {
            self.mapped_widths.borrow_mut().push(bytes);
            NonZeroUsize::new(phys_start).expect("test MMIO address must be non-zero")
        }

        fn unmap(&mut self, _virt_start: usize, _bytes: usize) {}
    }

    fn capability_parameters_with_pointer(pointer_dwords: u16) -> CapabilityParameters1 {
        let raw = u32::from(pointer_dwords) << 16;
        // SAFETY: CapabilityParameters1 is repr(transparent) over u32 and accepts every bit pattern.
        unsafe { core::mem::transmute::<u32, CapabilityParameters1>(raw) }
    }
}

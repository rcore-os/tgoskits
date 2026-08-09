//! Reusable LoongArch PCH-PIC device model.
//!
//! This module is a target-gated architecture device package. It provides a
//! guest-visible MMIO irqchip plus a narrow output-port service for the AxVM
//! LoongArch architecture layer; it is not part of the architecture-neutral
//! device runtime core.

use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(not(test))]
use ax_kspin::SpinNoIrq as Mutex;
#[cfg(test)]
use ax_kspin::SpinRaw as Mutex;
use axdevice_base::*;
use axvm_types::GuestPhysAddr;

use crate::*;
const PCH_PIC_INT_ID_LO: usize = 0x000;
const PCH_PIC_INT_ID_HI: usize = 0x004;
const PCH_PIC_INT_MASK_LO: usize = 0x020;
const PCH_PIC_INT_MASK_HI: usize = 0x024;
const PCH_PIC_HTMSI_EN_LO: usize = 0x040;
const PCH_PIC_HTMSI_EN_HI: usize = 0x044;
const PCH_PIC_INT_EDGE_LO: usize = 0x060;
const PCH_PIC_INT_EDGE_HI: usize = 0x064;
const PCH_PIC_INT_CLEAR_LO: usize = 0x080;
const PCH_PIC_INT_CLEAR_HI: usize = 0x084;
const PCH_PIC_AUTO_CTRL0_LO: usize = 0x0c0;
const PCH_PIC_AUTO_CTRL0_HI: usize = 0x0c4;
const PCH_PIC_AUTO_CTRL1_LO: usize = 0x0e0;
const PCH_PIC_AUTO_CTRL1_HI: usize = 0x0e4;
const PCH_PIC_ROUTE_ENTRY_BASE: usize = 0x100;
const PCH_PIC_HTMSI_VEC_BASE: usize = 0x200;
const PCH_PIC_INT_IRR_LO: usize = 0x380;
const PCH_PIC_INT_IRR_HI: usize = 0x384;
const PCH_PIC_INT_ISR_LO: usize = 0x3a0;
const PCH_PIC_INT_ISR_HI: usize = 0x3a4;
const PCH_PIC_POL_LO: usize = 0x3e0;
const PCH_PIC_POL_HI: usize = 0x3e4;
const PCH_PIC_IRQ_COUNT: usize = 64;
const PCH_PIC_INT_ID_VAL: usize = 0x0700_0000;
const PCH_PIC_INT_ID_VER: usize = 0x1;
const PCH_PIC_IO_LOG_LIMIT: usize = 256;
const PCH_PIC_IRQ_LOG_LIMIT: usize = 64;
const PCH_PIC_LEVEL_LOG_LIMIT: usize = 64;
const PCH_PIC_OUTPUT_QUEUE_CAPACITY: usize = 16;

static PCH_PIC_IO_LOGS: AtomicUsize = AtomicUsize::new(0);
static PCH_PIC_IRQ_LOGS: AtomicUsize = AtomicUsize::new(0);
static PCH_PIC_LEVEL_LOGS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug)]
struct PchPicState {
    int_mask: u64,
    htmsi_en: u64,
    intedge: u64,
    last_intirr: u64,
    intirr: u64,
    intisr: u64,
    int_polarity: u64,
    auto_ctrl0: u64,
    auto_ctrl1: u64,
    route_entry: [u8; PCH_PIC_IRQ_COUNT],
    htmsi_vector: [u8; PCH_PIC_IRQ_COUNT],
    output_events: [Option<PchPicOutputEvent>; PCH_PIC_OUTPUT_QUEUE_CAPACITY],
    output_head: usize,
    output_len: usize,
}

impl Default for PchPicState {
    fn default() -> Self {
        let mut state = Self {
            int_mask: !0,
            htmsi_en: 0,
            intedge: 0,
            last_intirr: 0,
            intirr: 0,
            intisr: 0,
            int_polarity: 0,
            auto_ctrl0: 0,
            auto_ctrl1: 0,
            route_entry: [0; PCH_PIC_IRQ_COUNT],
            htmsi_vector: [0; PCH_PIC_IRQ_COUNT],
            output_events: [None; PCH_PIC_OUTPUT_QUEUE_CAPACITY],
            output_head: 0,
            output_len: 0,
        };
        for irq in 0..PCH_PIC_IRQ_COUNT {
            state.route_entry[irq] = 1;
            state.htmsi_vector[irq] = irq as u8;
        }
        state
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PchPicOutputEvent {
    pub vector: usize,
    pub asserted: bool,
}

/// Architecture port through which LoongArch consumes PCH-PIC output.
///
/// The guest-visible PCH-PIC remains a normal MMIO device.  This narrow port
/// is only for the architecture to feed physical IRQs into it and to drain
/// output vectors produced by guest MMIO configuration.
pub trait PchPicOutputPort: Send + Sync {
    /// Updates one PCH-PIC input level and returns the routed EIOINTC vector.
    fn set_input_level(&self, irq: usize, asserted: bool) -> Option<usize>;

    /// Takes the next output event produced by the PCH-PIC.
    fn take_output_event(&self) -> Option<PchPicOutputEvent>;
}

/// Type key for the VM-local LoongArch PCH-PIC output port.
pub struct PchPicOutputPortKey;

impl ServiceKey for PchPicOutputPortKey {
    type Service = dyn PchPicOutputPort;

    const NAME: &'static str = "loongarch-pch-pic-output-port";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// Minimal LS7A PCH-PIC model for LoongArch QEMU virt guests.
///
/// Linux configures this irqchip through ACPI even when the backing PCI devices
/// are passthrough. The model must preserve the mask/IRR/ISR/route state so the
/// guest sees a coherent interrupt controller instead of changing the host PCH.
pub struct LoongArchPchPic {
    base: GuestPhysAddr,
    size: usize,
    resources: Box<[Resource]>,
    state: Mutex<PchPicState>,
}

impl LoongArchPchPic {
    pub fn new(base: GuestPhysAddr, size: usize) -> Self {
        Self {
            base,
            size,
            resources: alloc::vec![Resource::MmioRange {
                base: base.as_usize() as u64,
                size: size as u64,
            }]
            .into_boxed_slice(),
            state: Mutex::new(PchPicState::default()),
        }
    }

    /// Updates a PCH input source level and returns the EIOINTC source to assert, if any.
    pub fn set_irq_level(&self, irq: usize, level: bool) -> Option<usize> {
        let mut state = self.state.lock();
        if irq >= PCH_PIC_IRQ_COUNT {
            return None;
        }

        let mask = 1u64 << irq;
        if level {
            state.intirr |= mask;
            state.last_intirr |= mask;
        } else {
            state.intirr &= !mask;
            state.last_intirr &= !mask;
        }

        let routed = update_irq(&mut state, mask, level);
        log_pch_pic_level(&state, irq, level, routed);
        routed
    }

    /// Returns the pending EIOINTC source for an already-latched PCH source.
    pub fn pending_vector(&self, irq: usize) -> Option<usize> {
        let mut state = self.state.lock();
        if irq >= PCH_PIC_IRQ_COUNT {
            return None;
        }

        update_irq(&mut state, 1u64 << irq, true)
    }

    /// Drains output-line events generated by MMIO register writes.
    pub fn drain_output_events(&self, mut f: impl FnMut(PchPicOutputEvent)) {
        loop {
            let event = {
                let mut state = self.state.lock();
                pop_output_event(&mut state)
            };
            match event {
                Some(event) => f(event),
                None => return,
            }
        }
    }

    fn take_output_event(&self) -> Option<PchPicOutputEvent> {
        let mut state = self.state.lock();
        pop_output_event(&mut state)
    }

    fn contains(&self, addr: GuestPhysAddr) -> bool {
        let base = self.base.as_usize();
        let end = base.saturating_add(self.size);
        let addr = addr.as_usize();
        addr >= base && addr < end
    }

    /// Reads a guest-visible PCH-PIC MMIO register.
    pub fn read_register(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        if !self.contains(addr) {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        }
        let offset = addr.as_usize() - self.base.as_usize();
        let state = self.state.lock();
        let value = match width {
            AccessWidth::Byte => read_byte(&state, offset),
            AccessWidth::Word => read_split_bytes(&state, offset, 2),
            AccessWidth::Dword => read_dword(&state, offset),
            AccessWidth::Qword => {
                read_dword(&state, offset) | (read_dword(&state, offset + 4) << 32)
            }
        };
        log_pch_pic_io("read", offset, width, value);
        Ok(value)
    }

    /// Writes a guest-visible PCH-PIC MMIO register.
    pub fn write_register(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> DeviceResult {
        if !self.contains(addr) {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        }
        let offset = addr.as_usize() - self.base.as_usize();
        let mut state = self.state.lock();
        log_pch_pic_io("write", offset, width, val);
        match width {
            AccessWidth::Byte => write_byte(&mut state, offset, val as u8),
            AccessWidth::Word => write_split_bytes(&mut state, offset, 2, val),
            AccessWidth::Dword => write_dword(&mut state, offset, val as u32),
            AccessWidth::Qword => {
                write_dword(&mut state, offset, val as u32);
                write_dword(&mut state, offset + 4, (val >> 32) as u32);
            }
        }
        Ok(())
    }
}

impl PchPicOutputPort for LoongArchPchPic {
    fn set_input_level(&self, irq: usize, asserted: bool) -> Option<usize> {
        self.set_irq_level(irq, asserted)
    }

    fn take_output_event(&self) -> Option<PchPicOutputEvent> {
        self.take_output_event()
    }
}

/// Factory for the guest-visible LoongArch PCH-PIC contribution.
pub struct LoongArchPchPicFactory {
    base: usize,
    length: usize,
    domain_factory: Arc<dyn LoongArchInterruptDomainFactory>,
}

/// Architecture adapter that creates the VM-local cascaded interrupt domain.
pub trait LoongArchInterruptDomainFactory: Send + Sync {
    /// Creates a controller for one freshly reset PCH-PIC instance.
    fn create(
        &self,
        pic: Arc<LoongArchPchPic>,
    ) -> Arc<dyn axdevice_base::VirtualInterruptController>;
}

impl LoongArchPchPicFactory {
    /// Creates the only factory for an architecture-owned PCH-PIC instance.
    pub fn new(
        base: usize,
        length: usize,
        domain_factory: Arc<dyn LoongArchInterruptDomainFactory>,
    ) -> Self {
        Self {
            base,
            length,
            domain_factory,
        }
    }
}

impl DeviceModel for LoongArchPchPicFactory {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new().with_mmio(
            ResourceSlot::new("registers")?,
            self.length as u64,
            1,
            ResourceRequest::Fixed(self.base as u64),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let (base, length) = context.mmio(&ResourceSlot::new("registers")?)?;
        if base != self.base as u64 || length != self.length as u64 {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build LoongArch virtual PCH-PIC",
                detail: "planned MMIO range differs from the machine descriptor".into(),
            });
        }
        let base = usize::try_from(base).map_err(|_| DeviceManagerError::InvalidConfig {
            operation: "build LoongArch virtual PCH-PIC",
            detail: "planned MMIO base does not fit the target address width".into(),
        })?;
        let length = usize::try_from(length).map_err(|_| DeviceManagerError::InvalidConfig {
            operation: "build LoongArch virtual PCH-PIC",
            detail: "planned MMIO length does not fit the target address width".into(),
        })?;
        let pic = Arc::new(LoongArchPchPic::new(base.into(), length));
        let interrupt_controller = self.domain_factory.create(pic.clone());
        let device: Arc<dyn Device> = pic.clone();
        let output: Arc<dyn PchPicOutputPort> = pic;
        let mut bundle = DeviceBundle::from_registration(DeviceRegistration::Device(device))
            .with_service::<PchPicOutputPortKey>(output)?;
        bundle.push(DeviceRegistration::InterruptController(
            ControllerRegistration::new(interrupt_controller.id(), interrupt_controller),
        ));
        Ok(bundle)
    }
}

impl Device for LoongArchPchPic {
    fn name(&self) -> &str {
        "loongarch-pch-pic"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let addr = GuestPhysAddr::from_usize(access.addr as usize);
        if !self.contains(addr) {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }

        if access.is_read {
            self.read_register(addr, access.width)
                .map(|value| BusResponse::Read {
                    value: value as u64,
                })
        } else {
            self.write_register(addr, access.width, access.data as usize)
                .map(|_| BusResponse::Write)
        }
    }
}

fn read_byte(state: &PchPicState, offset: usize) -> usize {
    if let Some(index) = reg8_offset(offset) {
        return match index {
            PCH_PIC_ROUTE_ENTRY_BASE..=0x13f => pch_pic_irq_index(index, PCH_PIC_ROUTE_ENTRY_BASE)
                .map(|irq| state.route_entry[irq] as usize)
                .unwrap_or(0),
            PCH_PIC_HTMSI_VEC_BASE..=0x23f => pch_pic_irq_index(index, PCH_PIC_HTMSI_VEC_BASE)
                .map(|irq| state.htmsi_vector[irq] as usize)
                .unwrap_or(0),
            _ => 0,
        };
    }

    let shift = (offset & 0x3) * 8;
    (read_dword(state, offset & !0x3) >> shift) & 0xff
}

fn write_byte(state: &mut PchPicState, offset: usize, val: u8) {
    if let Some(index) = reg8_offset(offset) {
        match index {
            PCH_PIC_ROUTE_ENTRY_BASE..=0x13f => {
                if let Some(irq) = pch_pic_irq_index(index, PCH_PIC_ROUTE_ENTRY_BASE) {
                    state.route_entry[irq] = val;
                }
            }
            PCH_PIC_HTMSI_VEC_BASE..=0x23f => {
                if let Some(irq) = pch_pic_irq_index(index, PCH_PIC_HTMSI_VEC_BASE) {
                    state.htmsi_vector[irq] = val;
                }
            }
            _ => {}
        }
        return;
    }

    let aligned = offset & !0x3;
    let shift = (offset & 0x3) * 8;
    let old = read_dword(state, aligned);
    let new = (old & !(0xff << shift)) | ((val as usize) << shift);
    write_dword(state, aligned, new as u32);
}

fn read_split_bytes(state: &PchPicState, offset: usize, len: usize) -> usize {
    let mut value = 0;
    for idx in 0..len {
        value |= read_byte(state, offset + idx) << (idx * 8);
    }
    value
}

fn write_split_bytes(state: &mut PchPicState, offset: usize, len: usize, val: usize) {
    for idx in 0..len {
        write_byte(state, offset + idx, (val >> (idx * 8)) as u8);
    }
}

fn read_dword(state: &PchPicState, offset: usize) -> usize {
    match offset {
        PCH_PIC_INT_ID_LO => PCH_PIC_INT_ID_VAL,
        PCH_PIC_INT_ID_HI => PCH_PIC_INT_ID_VER | ((PCH_PIC_IRQ_COUNT - 1) << 16),
        PCH_PIC_INT_MASK_LO => state.int_mask as u32 as usize,
        PCH_PIC_INT_MASK_HI => (state.int_mask >> 32) as u32 as usize,
        PCH_PIC_HTMSI_EN_LO => state.htmsi_en as u32 as usize,
        PCH_PIC_HTMSI_EN_HI => (state.htmsi_en >> 32) as u32 as usize,
        PCH_PIC_INT_EDGE_LO => state.intedge as u32 as usize,
        PCH_PIC_INT_EDGE_HI => (state.intedge >> 32) as u32 as usize,
        PCH_PIC_AUTO_CTRL0_LO => state.auto_ctrl0 as u32 as usize,
        PCH_PIC_AUTO_CTRL0_HI => (state.auto_ctrl0 >> 32) as u32 as usize,
        PCH_PIC_AUTO_CTRL1_LO => state.auto_ctrl1 as u32 as usize,
        PCH_PIC_AUTO_CTRL1_HI => (state.auto_ctrl1 >> 32) as u32 as usize,
        PCH_PIC_INT_IRR_LO => state.intirr as u32 as usize,
        PCH_PIC_INT_IRR_HI => (state.intirr >> 32) as u32 as usize,
        PCH_PIC_INT_ISR_LO => (state.intisr & !state.int_mask) as u32 as usize,
        PCH_PIC_INT_ISR_HI => ((state.intisr & !state.int_mask) >> 32) as u32 as usize,
        PCH_PIC_POL_LO => state.int_polarity as u32 as usize,
        PCH_PIC_POL_HI => (state.int_polarity >> 32) as u32 as usize,
        PCH_PIC_ROUTE_ENTRY_BASE..=0x13f | PCH_PIC_HTMSI_VEC_BASE..=0x23f => {
            read_split_bytes(state, offset, 4)
        }
        _ => 0,
    }
}

fn write_dword(state: &mut PchPicState, offset: usize, val: u32) {
    match offset {
        PCH_PIC_INT_MASK_LO => update_int_mask(state, val, false),
        PCH_PIC_INT_MASK_HI => update_int_mask(state, val, true),
        PCH_PIC_HTMSI_EN_LO => state.htmsi_en = replace_u32(state.htmsi_en, val, false),
        PCH_PIC_HTMSI_EN_HI => state.htmsi_en = replace_u32(state.htmsi_en, val, true),
        PCH_PIC_INT_EDGE_LO => state.intedge = replace_u32(state.intedge, val, false),
        PCH_PIC_INT_EDGE_HI => state.intedge = replace_u32(state.intedge, val, true),
        PCH_PIC_INT_CLEAR_LO => clear_irq(state, val as u64),
        PCH_PIC_INT_CLEAR_HI => clear_irq(state, (val as u64) << 32),
        PCH_PIC_AUTO_CTRL0_LO => state.auto_ctrl0 = replace_u32(state.auto_ctrl0, val, false),
        PCH_PIC_AUTO_CTRL0_HI => state.auto_ctrl0 = replace_u32(state.auto_ctrl0, val, true),
        PCH_PIC_AUTO_CTRL1_LO => state.auto_ctrl1 = replace_u32(state.auto_ctrl1, val, false),
        PCH_PIC_AUTO_CTRL1_HI => state.auto_ctrl1 = replace_u32(state.auto_ctrl1, val, true),
        PCH_PIC_INT_ISR_LO => state.intisr = replace_u32(state.intisr, val, false),
        PCH_PIC_INT_ISR_HI => state.intisr = replace_u32(state.intisr, val, true),
        PCH_PIC_POL_LO => state.int_polarity = replace_u32(state.int_polarity, val, false),
        PCH_PIC_POL_HI => state.int_polarity = replace_u32(state.int_polarity, val, true),
        PCH_PIC_ROUTE_ENTRY_BASE..=0x13f | PCH_PIC_HTMSI_VEC_BASE..=0x23f => {
            write_split_bytes(state, offset, 4, val as usize)
        }
        _ => {}
    }
}

fn update_irq(state: &mut PchPicState, mask: u64, level: bool) -> Option<usize> {
    let valid_irqs = if PCH_PIC_IRQ_COUNT >= u64::BITS as usize {
        u64::MAX
    } else {
        (1u64 << PCH_PIC_IRQ_COUNT) - 1
    };
    let mask = mask & valid_irqs;
    if mask == 0 {
        return None;
    }

    if level {
        let pending = mask & state.intirr & !state.int_mask & !state.intisr;
        if pending != 0 {
            let irq = pending.trailing_zeros() as usize;
            state.intisr |= 1u64 << irq;
            log_pch_pic_irq(state, "assert", irq, level, mask);
            return Some(state.htmsi_vector[irq] as usize);
        }
    } else {
        let inactive = mask & state.intisr & !state.intirr;
        if inactive != 0 {
            let irq = inactive.trailing_zeros() as usize;
            state.intisr &= !(1u64 << irq);
            log_pch_pic_irq(state, "deassert", irq, level, mask);
            return Some(state.htmsi_vector[irq] as usize);
        }
    }

    None
}

fn pch_pic_irq_index(offset: usize, base: usize) -> Option<usize> {
    let irq = offset - base;
    (irq < PCH_PIC_IRQ_COUNT).then_some(irq)
}

fn reg8_offset(offset: usize) -> Option<usize> {
    (PCH_PIC_ROUTE_ENTRY_BASE..PCH_PIC_INT_ISR_LO)
        .contains(&offset)
        .then_some(offset)
}

fn clear_irq(state: &mut PchPicState, mask: u64) {
    let active = state.intisr & mask;
    state.intirr &= !mask;
    state.last_intirr &= !mask;
    state.intisr &= !mask;
    queue_events_for_mask(state, active, false);
}

fn update_int_mask(state: &mut PchPicState, val: u32, high: bool) {
    let old = state.int_mask;
    state.int_mask = replace_u32(old, val, high);

    let old_part = if high { old >> 32 } else { old } as u32;
    let newly_unmasked = old_part & !val;
    if newly_unmasked != 0 {
        let mask = if high {
            (newly_unmasked as u64) << 32
        } else {
            newly_unmasked as u64
        };
        if let Some(vector) = update_irq(state, mask, true) {
            push_output_event(
                state,
                PchPicOutputEvent {
                    vector,
                    asserted: true,
                },
            );
        }
    }

    let newly_masked = !old_part & val;
    if newly_masked != 0 {
        let mask = if high {
            (newly_masked as u64) << 32
        } else {
            newly_masked as u64
        };
        // Masking disconnects the PCH source from EIOINTC even if its input
        // level remains high. Keep `intirr` latched so a later unmask can
        // assert it again, but retract every output line already in service.
        let active = state.intisr & mask;
        state.intisr &= !active;
        queue_events_for_mask(state, active, false);
    }
}

fn queue_events_for_mask(state: &mut PchPicState, mut mask: u64, asserted: bool) {
    while mask != 0 {
        let irq = mask.trailing_zeros() as usize;
        push_output_event(
            state,
            PchPicOutputEvent {
                vector: state.htmsi_vector[irq] as usize,
                asserted,
            },
        );
        mask &= !(1u64 << irq);
    }
}

fn push_output_event(state: &mut PchPicState, event: PchPicOutputEvent) {
    if state.output_len == PCH_PIC_OUTPUT_QUEUE_CAPACITY {
        warn!(
            "LoongArch PCH-PIC output event queue full, dropping event {:?}",
            event
        );
        return;
    }
    let index = (state.output_head + state.output_len) % PCH_PIC_OUTPUT_QUEUE_CAPACITY;
    state.output_events[index] = Some(event);
    state.output_len += 1;
}

fn pop_output_event(state: &mut PchPicState) -> Option<PchPicOutputEvent> {
    if state.output_len == 0 {
        return None;
    }

    let event = state.output_events[state.output_head].take();
    state.output_head = (state.output_head + 1) % PCH_PIC_OUTPUT_QUEUE_CAPACITY;
    state.output_len -= 1;
    event
}

fn replace_u32(old: u64, val: u32, high: bool) -> u64 {
    if high {
        (old & 0x0000_0000_ffff_ffff) | ((val as u64) << 32)
    } else {
        (old & 0xffff_ffff_0000_0000) | val as u64
    }
}

fn log_pch_pic_io(op: &str, offset: usize, width: AccessWidth, value: usize) {
    let is_key_reg = matches!(
        offset,
        PCH_PIC_INT_MASK_LO
            | PCH_PIC_INT_MASK_HI
            | PCH_PIC_INT_CLEAR_LO
            | PCH_PIC_INT_CLEAR_HI
            | PCH_PIC_HTMSI_EN_LO
            | PCH_PIC_HTMSI_EN_HI
    );
    if is_key_reg || PCH_PIC_IO_LOGS.fetch_add(1, Ordering::Relaxed) < PCH_PIC_IO_LOG_LIMIT {
        trace!(
            "LoongArch guest PCH-PIC {op}: offset={:#x}, width={:?}, value={:#x}",
            offset, width, value
        );
    }
}

fn log_pch_pic_level(state: &PchPicState, irq: usize, level: bool, routed: Option<usize>) {
    if PCH_PIC_LEVEL_LOGS.fetch_add(1, Ordering::Relaxed) < PCH_PIC_LEVEL_LOG_LIMIT {
        trace!(
            "LoongArch guest PCH-PIC level: input={}, level={}, routed={:?}, int_mask={:#x}, \
             intirr={:#x}, intisr={:#x}, htvec={}",
            irq, level, routed, state.int_mask, state.intirr, state.intisr, state.htmsi_vector[irq]
        );
    }
}

fn log_pch_pic_irq(state: &PchPicState, op: &str, irq: usize, level: bool, mask: u64) {
    if PCH_PIC_IRQ_LOGS.fetch_add(1, Ordering::Relaxed) < PCH_PIC_IRQ_LOG_LIMIT {
        trace!(
            "LoongArch guest PCH-PIC irq {op}: input={}, level={}, mask={:#x}, int_mask={:#x}, \
             intirr={:#x}, intisr={:#x}, htvec={}",
            irq, level, mask, state.int_mask, state.intirr, state.intisr, state.htmsi_vector[irq]
        );
    }
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;

    #[test]
    fn unmask_latched_irq_emits_assert_event() {
        let pic = LoongArchPchPic::new(GuestPhysAddr::from_usize(0x1000), 0x1000);
        assert_eq!(pic.set_irq_level(5, true), None);

        pic.write_register(
            GuestPhysAddr::from_usize(0x1000 + PCH_PIC_INT_MASK_LO),
            AccessWidth::Dword,
            !(1u32 << 5) as usize,
        )
        .unwrap();

        let mut events = Vec::new();
        pic.drain_output_events(|event| events.push(event));
        assert_eq!(
            events,
            vec![PchPicOutputEvent {
                vector: 5,
                asserted: true
            }]
        );
    }

    #[test]
    fn clear_asserted_irq_emits_deassert_event() {
        let pic = LoongArchPchPic::new(GuestPhysAddr::from_usize(0x1000), 0x1000);
        pic.write_register(
            GuestPhysAddr::from_usize(0x1000 + PCH_PIC_INT_MASK_LO),
            AccessWidth::Dword,
            !(1u32 << 5) as usize,
        )
        .unwrap();
        assert_eq!(pic.set_irq_level(5, true), Some(5));

        pic.write_register(
            GuestPhysAddr::from_usize(0x1000 + PCH_PIC_INT_CLEAR_LO),
            AccessWidth::Dword,
            (1u32 << 5) as usize,
        )
        .unwrap();

        let mut events = Vec::new();
        pic.drain_output_events(|event| events.push(event));
        assert_eq!(
            events,
            vec![PchPicOutputEvent {
                vector: 5,
                asserted: false
            }]
        );
    }

    #[test]
    fn output_port_preserves_deassert_then_reassert_order() {
        let pic = LoongArchPchPic::new(GuestPhysAddr::from_usize(0x1000), 0x1000);
        let irq = 5;
        let address = GuestPhysAddr::from_usize(0x1000 + PCH_PIC_INT_MASK_LO);

        pic.write_register(address, AccessWidth::Dword, !(1u32 << irq) as usize)
            .unwrap();
        assert_eq!(pic.set_input_level(irq, true), Some(irq));

        // Masking an active level produces a deassert event. Re-enabling the
        // same still-latched source must queue a later assert, not discard it.
        pic.write_register(address, AccessWidth::Dword, usize::MAX)
            .unwrap();
        pic.write_register(address, AccessWidth::Dword, !(1u32 << irq) as usize)
            .unwrap();

        let mut events = Vec::new();
        while let Some(event) = PchPicOutputPort::take_output_event(&pic) {
            events.push(event);
        }
        assert_eq!(
            events,
            vec![
                PchPicOutputEvent {
                    vector: irq,
                    asserted: false,
                },
                PchPicOutputEvent {
                    vector: irq,
                    asserted: true,
                },
            ]
        );
    }
}

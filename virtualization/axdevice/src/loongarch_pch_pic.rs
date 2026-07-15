use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(target_arch = "loongarch64")]
use ax_kspin::SpinNoIrq as Mutex;
#[cfg(target_arch = "loongarch64")]
use axdevice_base::{AccessWidth, BaseDeviceOps, DeviceResult, EmuDeviceType};
#[cfg(target_arch = "loongarch64")]
use axvm_types::{GuestPhysAddr, GuestPhysAddrRange};

#[cfg(target_arch = "loongarch64")]
use crate::DeviceManagerResult;

const PCH_PIC_IRQ_COUNT: usize = 64;
const PCH_PIC_IRQ_LOG_LIMIT: usize = 64;
// A single 64-bit clear may deassert every PCH input before AxVM drains the output.
const PCH_PIC_OUTPUT_QUEUE_CAPACITY: usize = PCH_PIC_IRQ_COUNT;

static PCH_PIC_IRQ_LOGS: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_arch = "loongarch64")]
mod device_registers {
    use super::{AtomicUsize, Ordering};

    pub(super) const PCH_PIC_INT_ID_LO: usize = 0x000;
    pub(super) const PCH_PIC_INT_ID_HI: usize = 0x004;
    pub(super) const PCH_PIC_INT_MASK_LO: usize = 0x020;
    pub(super) const PCH_PIC_INT_MASK_HI: usize = 0x024;
    pub(super) const PCH_PIC_HTMSI_EN_LO: usize = 0x040;
    pub(super) const PCH_PIC_HTMSI_EN_HI: usize = 0x044;
    pub(super) const PCH_PIC_INT_EDGE_LO: usize = 0x060;
    pub(super) const PCH_PIC_INT_EDGE_HI: usize = 0x064;
    pub(super) const PCH_PIC_INT_CLEAR_LO: usize = 0x080;
    pub(super) const PCH_PIC_INT_CLEAR_HI: usize = 0x084;
    pub(super) const PCH_PIC_AUTO_CTRL0_LO: usize = 0x0c0;
    pub(super) const PCH_PIC_AUTO_CTRL0_HI: usize = 0x0c4;
    pub(super) const PCH_PIC_AUTO_CTRL1_LO: usize = 0x0e0;
    pub(super) const PCH_PIC_AUTO_CTRL1_HI: usize = 0x0e4;
    pub(super) const PCH_PIC_ROUTE_ENTRY_BASE: usize = 0x100;
    pub(super) const PCH_PIC_HTMSI_VEC_BASE: usize = 0x200;
    pub(super) const PCH_PIC_INT_IRR_LO: usize = 0x380;
    pub(super) const PCH_PIC_INT_IRR_HI: usize = 0x384;
    pub(super) const PCH_PIC_INT_ISR_LO: usize = 0x3a0;
    pub(super) const PCH_PIC_INT_ISR_HI: usize = 0x3a4;
    pub(super) const PCH_PIC_POL_LO: usize = 0x3e0;
    pub(super) const PCH_PIC_POL_HI: usize = 0x3e4;
    pub(super) const PCH_PIC_INT_ID_VAL: usize = 0x0700_0000;
    pub(super) const PCH_PIC_INT_ID_VER: usize = 0x1;
    pub(super) const PCH_PIC_IO_LOG_LIMIT: usize = 256;
    pub(super) const PCH_PIC_LEVEL_LOG_LIMIT: usize = 64;

    pub(super) static PCH_PIC_IO_LOGS: AtomicUsize = AtomicUsize::new(0);
    pub(super) static PCH_PIC_LEVEL_LOGS: AtomicUsize = AtomicUsize::new(0);

    pub(super) fn should_log_io() -> bool {
        PCH_PIC_IO_LOGS.fetch_add(1, Ordering::Relaxed) < PCH_PIC_IO_LOG_LIMIT
    }

    pub(super) fn should_log_level() -> bool {
        PCH_PIC_LEVEL_LOGS.fetch_add(1, Ordering::Relaxed) < PCH_PIC_LEVEL_LOG_LIMIT
    }
}

#[cfg(target_arch = "loongarch64")]
use device_registers::*;

#[derive(Clone, Debug)]
struct PchPicState {
    int_mask: u64,
    #[cfg(target_arch = "loongarch64")]
    htmsi_en: u64,
    #[cfg(target_arch = "loongarch64")]
    intedge: u64,
    last_intirr: u64,
    intirr: u64,
    intisr: u64,
    #[cfg(target_arch = "loongarch64")]
    int_polarity: u64,
    #[cfg(target_arch = "loongarch64")]
    auto_ctrl0: u64,
    #[cfg(target_arch = "loongarch64")]
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
            #[cfg(target_arch = "loongarch64")]
            htmsi_en: 0,
            #[cfg(target_arch = "loongarch64")]
            intedge: 0,
            last_intirr: 0,
            intirr: 0,
            intisr: 0,
            #[cfg(target_arch = "loongarch64")]
            int_polarity: 0,
            #[cfg(target_arch = "loongarch64")]
            auto_ctrl0: 0,
            #[cfg(target_arch = "loongarch64")]
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
    vector: usize,
    asserted: bool,
}

impl PchPicOutputEvent {
    /// Returns the EIOINTC vector driven by the PCH-PIC output.
    pub const fn vector(self) -> usize {
        self.vector
    }

    /// Returns whether the PCH-PIC output is asserted.
    pub const fn is_asserted(self) -> bool {
        self.asserted
    }
}

/// Runtime operations supplied by the VM's PCH-PIC to EIOINTC adapter.
#[cfg(target_arch = "loongarch64")]
pub trait LoongArchPchPicRuntimeOps: Send + Sync {
    /// Routes output events created by guest PCH-PIC register writes.
    fn service_output_events(&self) -> DeviceManagerResult;
}

/// Minimal LS7A PCH-PIC model for LoongArch QEMU virt guests.
///
/// Linux configures this irqchip through ACPI even when the backing PCI devices
/// are passthrough. The model must preserve the mask/IRR/ISR/route state so the
/// guest sees a coherent interrupt controller instead of changing the host PCH.
#[cfg(target_arch = "loongarch64")]
pub struct LoongArchPchPic {
    base: GuestPhysAddr,
    size: usize,
    state: Mutex<PchPicState>,
}

#[cfg(target_arch = "loongarch64")]
impl LoongArchPchPic {
    pub fn new(base: GuestPhysAddr, size: usize) -> Self {
        Self {
            base,
            size,
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
}

#[cfg(target_arch = "loongarch64")]
impl BaseDeviceOps<GuestPhysAddrRange> for LoongArchPchPic {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::LoongArchPchPic
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.base, self.size)
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
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

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> DeviceResult {
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

#[cfg(target_arch = "loongarch64")]
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

#[cfg(target_arch = "loongarch64")]
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

#[cfg(target_arch = "loongarch64")]
fn read_split_bytes(state: &PchPicState, offset: usize, len: usize) -> usize {
    let mut value = 0;
    for idx in 0..len {
        value |= read_byte(state, offset + idx) << (idx * 8);
    }
    value
}

#[cfg(target_arch = "loongarch64")]
fn write_split_bytes(state: &mut PchPicState, offset: usize, len: usize, val: usize) {
    for idx in 0..len {
        write_byte(state, offset + idx, (val >> (idx * 8)) as u8);
    }
}

#[cfg(target_arch = "loongarch64")]
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

#[cfg(target_arch = "loongarch64")]
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

#[cfg(target_arch = "loongarch64")]
fn pch_pic_irq_index(offset: usize, base: usize) -> Option<usize> {
    let irq = offset - base;
    (irq < PCH_PIC_IRQ_COUNT).then_some(irq)
}

#[cfg(target_arch = "loongarch64")]
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
        if let Some(vector) = update_irq(state, mask, false) {
            push_output_event(
                state,
                PchPicOutputEvent {
                    vector,
                    asserted: false,
                },
            );
        }
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

#[cfg(target_arch = "loongarch64")]
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
    if is_key_reg || should_log_io() {
        trace!(
            "LoongArch guest PCH-PIC {op}: offset={:#x}, width={:?}, value={:#x}",
            offset, width, value
        );
    }
}

#[cfg(target_arch = "loongarch64")]
fn log_pch_pic_level(state: &PchPicState, irq: usize, level: bool, routed: Option<usize>) {
    if should_log_level() {
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
        let mut state = PchPicState::default();
        let input = 1u64 << 5;
        state.intirr |= input;
        state.last_intirr |= input;
        assert_eq!(update_irq(&mut state, input, true), None);

        update_int_mask(&mut state, !(input as u32), false);

        let mut events = Vec::new();
        while let Some(event) = pop_output_event(&mut state) {
            events.push(event);
        }
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
        let mut state = PchPicState::default();
        let input = 1u64 << 5;
        update_int_mask(&mut state, !(input as u32), false);
        state.intirr |= input;
        state.last_intirr |= input;
        assert_eq!(update_irq(&mut state, input, true), Some(5));

        clear_irq(&mut state, input);

        let mut events = Vec::new();
        while let Some(event) = pop_output_event(&mut state) {
            events.push(event);
        }
        assert_eq!(
            events,
            vec![PchPicOutputEvent {
                vector: 5,
                asserted: false
            }]
        );
    }

    #[test]
    fn clearing_all_active_inputs_preserves_every_output_event() {
        let mut state = PchPicState {
            int_mask: 0,
            ..PchPicState::default()
        };
        for input in 0..PCH_PIC_IRQ_COUNT {
            let mask = 1u64 << input;
            state.intirr |= mask;
            assert_eq!(update_irq(&mut state, mask, true), Some(input));
        }

        clear_irq(&mut state, u64::MAX);

        let mut events = Vec::new();
        while let Some(event) = pop_output_event(&mut state) {
            events.push(event);
        }
        assert_eq!(events.len(), PCH_PIC_IRQ_COUNT);
        assert!(events.iter().all(|event| !event.is_asserted()));
        assert!(
            events
                .iter()
                .copied()
                .map(PchPicOutputEvent::vector)
                .eq(0..PCH_PIC_IRQ_COUNT)
        );
    }
}

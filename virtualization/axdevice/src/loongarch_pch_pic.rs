use core::sync::atomic::{AtomicUsize, Ordering};

use ax_errno::AxResult;
use ax_kspin::SpinNoIrq as Mutex;
use axdevice_base::{AccessWidth, BaseDeviceOps, EmuDeviceType};
use axvm_types::{GuestPhysAddr, GuestPhysAddrRange};

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
        };
        for irq in 0..PCH_PIC_IRQ_COUNT {
            state.route_entry[irq] = 1;
            state.htmsi_vector[irq] = irq as u8;
        }
        state
    }
}

/// Minimal LS7A PCH-PIC model for LoongArch QEMU virt guests.
///
/// Linux configures this irqchip through ACPI even when the backing PCI devices
/// are passthrough. The model must preserve the mask/IRR/ISR/route state so the
/// guest sees a coherent interrupt controller instead of changing the host PCH.
pub struct LoongArchPchPic {
    base: GuestPhysAddr,
    size: usize,
    state: Mutex<PchPicState>,
}

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

    /// Returns the pending EIOINTC source for an already-latched PCH source.
    pub fn pending_vector(&self, irq: usize) -> Option<usize> {
        let mut state = self.state.lock();
        if irq >= PCH_PIC_IRQ_COUNT {
            return None;
        }

        update_irq(&mut state, 1u64 << irq, true)
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for LoongArchPchPic {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::LoongArchPchPic
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.base, self.size)
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
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

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> AxResult {
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

    match offset {
        _ => {
            let shift = (offset & 0x3) * 8;
            (read_dword(state, offset & !0x3) >> shift) & 0xff
        }
    }
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

    match offset {
        _ => {
            let aligned = offset & !0x3;
            let shift = (offset & 0x3) * 8;
            let old = read_dword(state, aligned);
            let new = (old & !(0xff << shift)) | ((val as usize) << shift);
            write_dword(state, aligned, new as u32);
        }
    }
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
    state.intirr &= !mask;
    state.last_intirr &= !mask;
    state.intisr &= !mask;
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
        let _ = update_irq(state, mask, true);
    }

    let newly_masked = !old_part & val;
    if newly_masked != 0 {
        let mask = if high {
            (newly_masked as u64) << 32
        } else {
            newly_masked as u64
        };
        let _ = update_irq(state, mask, false);
    }
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

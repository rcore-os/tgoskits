use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_cpu_local::CpuPin;

use crate::{
    context_frame::LoongArchContextFrame,
    guest_csr::inject_guest_interrupt_at,
    host::LoongArchHostOps,
    host_cpu::{
        host_eiointc_has_pending, host_iocsr_read_b, host_iocsr_read_d, host_iocsr_read_h,
        host_iocsr_read_w, host_iocsr_write_b, host_iocsr_write_d, host_iocsr_write_h,
        host_iocsr_write_w,
    },
    registers::{
        extioi_cpu_encode_enabled, extioi_features_value, iocsr_mbuf_send_box, iocsr_mbuf_send_buf,
        iocsr_mbuf_send_cpu, iocsr_send_action, iocsr_send_byte_mask, iocsr_send_cpu,
        iocsr_send_data,
    },
    trap::{
        HWI_MASK, INT_HWI0, INT_HWI7, INT_IPI, IPI_BIT, LOCAL_INTERRUPT_MASK, advance_guest_pc,
        decode_interrupt_vector, extract_field, get_guest_pc,
    },
    types::{LoongArchVcpuId, LoongArchVcpuResult, LoongArchVmExit, LoongArchVmId},
};

pub(crate) const EIOINTC_ISR_BASE: usize = 0x1800;
pub(crate) const EIOINTC_ISR_REG_COUNT: usize = 4;
pub(crate) const EIOINTC_ISR_END: usize = EIOINTC_ISR_BASE + EIOINTC_ISR_REG_COUNT * 8;
const EIOINTC_VECTOR_COUNT: usize = EIOINTC_ISR_REG_COUNT * 64;
const EIOINTC_HWI_BASE: usize = INT_HWI0;
const EIOINTC_NODEMAP_BASE: usize = 0x14a0;
const EIOINTC_NODEMAP_END: usize = 0x14c0;
const EIOINTC_IPMAP_BASE: usize = 0x14c0;
const EIOINTC_IPMAP_END: usize = 0x14c8;
const EIOINTC_ENABLE_BASE: usize = 0x1600;
const EIOINTC_ENABLE_END: usize = 0x1620;
const EIOINTC_BOUNCE_BASE: usize = 0x1680;
const EIOINTC_BOUNCE_END: usize = 0x16a0;
const EIOINTC_ISR_COMPAT_BASE: usize = 0x1700;
const EIOINTC_ISR_COMPAT_END: usize = 0x1720;
const EIOINTC_COREMAP_BASE: usize = 0x1c00;
const EIOINTC_COREMAP_END: usize = 0x1d00;
const EIOINTC_GUEST_OWNED_BASE: usize = 0x1400;
const EIOINTC_GUEST_OWNED_END: usize = 0x2000;
const LOONGARCH_IOCSR_IPI_STATUS: usize = 0x1000;
const LOONGARCH_IOCSR_IPI_EN: usize = 0x1004;
const LOONGARCH_IOCSR_IPI_SET: usize = 0x1008;
const LOONGARCH_IOCSR_IPI_CLEAR: usize = 0x100c;
const LOONGARCH_IOCSR_MAIL_BUF0: usize = 0x1020;
const LOONGARCH_IOCSR_MAIL_BUF3: usize = 0x1038;
const LOONGARCH_IOCSR_IPI_SEND: usize = 0x1040;
const LOONGARCH_IOCSR_MBUF_SEND: usize = 0x1048;
const LOONGARCH_IOCSR_ANY_SEND: usize = 0x1158;
const IOCSR_MBUF_SEND_H32_MASK: usize = 0xffff_ffff_0000_0000;
const IOCSR_MAIL_BUF_COUNT: usize = 4;
const EIOINTC_NODEMAP_WORDS: usize = 8;
const EIOINTC_IPMAP_WORDS: usize = 2;
const EIOINTC_ENABLE_WORDS_PER_VCPU: usize = 8;
const EIOINTC_BOUNCE_WORDS: usize = 8;
const EIOINTC_COREMAP_WORDS: usize = 64;
const EXTIOI_VIRT_FEATURES: usize = 0x4000_0000;
const EXTIOI_VIRT_CONFIG: usize = 0x4000_0004;

static IOCSR_EXIT_LOGS: AtomicUsize = AtomicUsize::new(0);
static EIOINTC_TRACE_LOGS: AtomicUsize = AtomicUsize::new(0);
static TARGET_IOCSR_LOGS: AtomicUsize = AtomicUsize::new(0);

pub type LoongArchIocsrStateRef = Arc<LoongArchIocsrState>;

#[derive(Debug)]
pub struct LoongArchIocsrState {
    vcpus: Box<[LoongArchVcpuIocsrState]>,
}

impl LoongArchIocsrState {
    pub fn new(vcpu_count: usize) -> LoongArchVcpuResult<LoongArchIocsrStateRef> {
        let mut vcpus = alloc::vec::Vec::with_capacity(vcpu_count);
        for _ in 0..vcpu_count {
            vcpus.push(LoongArchVcpuIocsrState::new());
        }
        Ok(Arc::new(Self {
            vcpus: vcpus.into_boxed_slice(),
        }))
    }

    fn vcpu(&self, vcpu_id: LoongArchVcpuId) -> Option<&LoongArchVcpuIocsrState> {
        self.vcpus.get(vcpu_id)
    }

    pub fn reset_vcpu(&self, vcpu_id: LoongArchVcpuId) {
        if let Some(vcpu) = self.vcpu(vcpu_id) {
            vcpu.reset();
        }
    }
}

#[derive(Debug)]
struct LoongArchVcpuIocsrState {
    ipi_status: AtomicUsize,
    ipi_enable: AtomicUsize,
    mail_buf: [AtomicUsize; IOCSR_MAIL_BUF_COUNT],
    eiointc_isr: [AtomicUsize; EIOINTC_ISR_REG_COUNT],
    eiointc_nodemap: [AtomicUsize; EIOINTC_NODEMAP_WORDS],
    eiointc_ipmap: [AtomicUsize; EIOINTC_IPMAP_WORDS],
    eiointc_enable: [AtomicUsize; EIOINTC_ENABLE_WORDS_PER_VCPU],
    eiointc_bounce: [AtomicUsize; EIOINTC_BOUNCE_WORDS],
    eiointc_coremap: [AtomicUsize; EIOINTC_COREMAP_WORDS],
    eiointc_virt_config: AtomicUsize,
}

impl LoongArchVcpuIocsrState {
    const fn new() -> Self {
        Self {
            ipi_status: AtomicUsize::new(0),
            ipi_enable: AtomicUsize::new(0),
            mail_buf: [const { AtomicUsize::new(0) }; IOCSR_MAIL_BUF_COUNT],
            eiointc_isr: [const { AtomicUsize::new(0) }; EIOINTC_ISR_REG_COUNT],
            eiointc_nodemap: [const { AtomicUsize::new(0) }; EIOINTC_NODEMAP_WORDS],
            eiointc_ipmap: [const { AtomicUsize::new(0) }; EIOINTC_IPMAP_WORDS],
            eiointc_enable: [const { AtomicUsize::new(0) }; EIOINTC_ENABLE_WORDS_PER_VCPU],
            eiointc_bounce: [const { AtomicUsize::new(0) }; EIOINTC_BOUNCE_WORDS],
            eiointc_coremap: [const { AtomicUsize::new(0) }; EIOINTC_COREMAP_WORDS],
            eiointc_virt_config: AtomicUsize::new(0),
        }
    }

    fn reset(&self) {
        self.ipi_status.store(0, Ordering::Release);
        self.ipi_enable.store(0, Ordering::Release);
        self.eiointc_virt_config.store(0, Ordering::Release);

        for slot in &self.mail_buf {
            slot.store(0, Ordering::Release);
        }
        for slot in &self.eiointc_isr {
            slot.store(0, Ordering::Release);
        }
        for slot in &self.eiointc_nodemap {
            slot.store(0, Ordering::Release);
        }
        for slot in &self.eiointc_ipmap {
            slot.store(0, Ordering::Release);
        }
        for slot in &self.eiointc_enable {
            slot.store(0, Ordering::Release);
        }
        for slot in &self.eiointc_bounce {
            slot.store(0, Ordering::Release);
        }
        for slot in &self.eiointc_coremap {
            slot.store(0, Ordering::Release);
        }
    }
}

fn is_eiointc_isr_addr(addr: usize) -> bool {
    (EIOINTC_ISR_BASE..EIOINTC_ISR_END).contains(&addr) && addr.is_multiple_of(8)
}

fn iocsr_mail_buf_index(addr: usize) -> Option<usize> {
    if !(LOONGARCH_IOCSR_MAIL_BUF0..=LOONGARCH_IOCSR_MAIL_BUF3).contains(&addr)
        || !(addr - LOONGARCH_IOCSR_MAIL_BUF0).is_multiple_of(8)
    {
        return None;
    }

    Some((addr - LOONGARCH_IOCSR_MAIL_BUF0) / 8)
}

fn read_atomic_u32_slots(slots: &[AtomicUsize], addr: usize, base: usize, len: usize) -> usize {
    let word = (addr - base) >> 2;
    let value = slots
        .get(word)
        .map(|slot| slot.load(Ordering::Acquire) as u32 as usize)
        .unwrap_or(0);
    if len == 8 {
        value
            | (slots
                .get(word + 1)
                .map(|slot| (slot.load(Ordering::Acquire) as u32 as usize) << 32)
                .unwrap_or(0))
    } else {
        value
    }
}

fn write_atomic_u32_slots(
    slots: &[AtomicUsize],
    addr: usize,
    base: usize,
    len: usize,
    value: usize,
) {
    let word = (addr - base) >> 2;
    if let Some(slot) = slots.get(word) {
        slot.store(value as u32 as usize, Ordering::Release);
    }
    if len == 8
        && let Some(slot) = slots.get(word + 1)
    {
        slot.store((value >> 32) as u32 as usize, Ordering::Release);
    }
}

fn eiointc_isr_word(vcpu: &LoongArchVcpuIocsrState, word: usize) -> usize {
    vcpu.eiointc_isr
        .get(word / 2)
        .map(|slot| {
            let value = slot.load(Ordering::Acquire);
            if word.is_multiple_of(2) {
                value as u32 as usize
            } else {
                (value >> 32) as u32 as usize
            }
        })
        .unwrap_or(0)
}

fn clear_eiointc_isr_word(vcpu: &LoongArchVcpuIocsrState, word: usize, value: usize) {
    if let Some(slot) = vcpu.eiointc_isr.get(word / 2) {
        let shift = (word % 2) * 32;
        let mask = (value as u32 as usize) << shift;
        slot.fetch_and(!mask, Ordering::AcqRel);
    }
}

fn read_eiointc_isr_slots(
    vcpu: &LoongArchVcpuIocsrState,
    addr: usize,
    base: usize,
    len: usize,
) -> usize {
    let word = (addr - base) >> 2;
    let value = eiointc_isr_word(vcpu, word);
    if len == 8 {
        value | (eiointc_isr_word(vcpu, word + 1) << 32)
    } else {
        value
    }
}

fn clear_eiointc_isr_slots(
    vcpu: &LoongArchVcpuIocsrState,
    addr: usize,
    base: usize,
    len: usize,
    value: usize,
) {
    let word = (addr - base) >> 2;
    clear_eiointc_isr_word(vcpu, word, value);
    if len == 8 {
        clear_eiointc_isr_word(vcpu, word + 1, value >> 32);
    }
}

fn eiointc_enable_word(vcpu: &LoongArchVcpuIocsrState, word: usize) -> usize {
    vcpu.eiointc_enable[word].load(Ordering::Acquire)
}

fn eiointc_ipmap_word(vcpu: &LoongArchVcpuIocsrState, word: usize) -> usize {
    vcpu.eiointc_ipmap[word].load(Ordering::Acquire)
}

fn eiointc_coremap_word(vcpu: &LoongArchVcpuIocsrState, word: usize) -> usize {
    vcpu.eiointc_coremap[word].load(Ordering::Acquire)
}

fn eiointc_virt_config(vcpu: &LoongArchVcpuIocsrState) -> usize {
    vcpu.eiointc_virt_config.load(Ordering::Acquire)
}

fn eiointc_group_for_source(vcpu: &LoongArchVcpuIocsrState, source: usize) -> Option<usize> {
    let ipmap_index = source >> 7;
    let ipmap_byte = (source >> 5) & 0x3;
    let ipmap = eiointc_ipmap_word(vcpu, ipmap_index);
    let pin_mask = (ipmap >> (ipmap_byte * 8)) & 0xff;
    if pin_mask == 0 {
        None
    } else {
        Some(pin_mask.trailing_zeros() as usize)
    }
}

fn eiointc_source_targets_vcpu0(vcpu: &LoongArchVcpuIocsrState, source: usize) -> bool {
    let word = source >> 2;
    let byte = source & 0x3;
    let route = (eiointc_coremap_word(vcpu, word) >> (byte * 8)) & 0xff;

    if extioi_cpu_encode_enabled(eiointc_virt_config(vcpu)) {
        route == 0 || route == 1
    } else {
        route & 0x1 != 0
    }
}

fn eiointc_cpu_irq_for_source(vcpu: &LoongArchVcpuIocsrState, source: usize) -> Option<usize> {
    let pin = eiointc_group_for_source(vcpu, source)?;
    if !eiointc_source_targets_vcpu0(vcpu, source) {
        return None;
    }

    if pin <= INT_HWI7 - INT_HWI0 {
        Some(INT_HWI0 + pin)
    } else {
        None
    }
}

fn guest_eiointc_pending_hwi(vcpu: &LoongArchVcpuIocsrState) -> Option<usize> {
    for word in 0..EIOINTC_ENABLE_WORDS_PER_VCPU {
        let pending = eiointc_isr_word(vcpu, word) & eiointc_enable_word(vcpu, word);
        if pending == 0 {
            continue;
        }
        for bit in 0..u32::BITS as usize {
            if pending & (1usize << bit) != 0 {
                let source = word * u32::BITS as usize + bit;
                if let Some(hwi) = eiointc_cpu_irq_for_source(vcpu, source) {
                    return Some(hwi);
                }
            }
        }
    }
    None
}

fn update_guest_eiointc_irq(
    cpu_pin: &CpuPin,
    ctx: &mut LoongArchContextFrame,
    vcpu: &LoongArchVcpuIocsrState,
) {
    let hwi_bits = HWI_MASK;
    if let Some(hwi) = guest_eiointc_pending_hwi(vcpu) {
        ctx.gcsr_estat = (ctx.gcsr_estat & !hwi_bits) | (1usize << hwi);
        crate::registers::set_hwi_interrupts(cpu_pin, 1usize << hwi);
    } else {
        ctx.gcsr_estat &= !hwi_bits;
        crate::registers::set_hwi_interrupts(cpu_pin, 0);
    }
}

pub(crate) fn init_guest_iocsr(state: &LoongArchIocsrState, vcpu_id: LoongArchVcpuId) {
    let Some(vcpu) = state.vcpu(vcpu_id) else {
        log::warn!("LoongArch guest IOCSR init skipped: VCpu[{vcpu_id}] is not in VM state");
        return;
    };

    vcpu.reset();

    for word in 0..EIOINTC_NODEMAP_WORDS {
        let value = ((1usize << (word * 2 + 1)) << 16) | (1usize << (word * 2));
        vcpu.eiointc_nodemap[word].store(value as u32 as usize, Ordering::Release);
    }
    for word in 0..EIOINTC_ENABLE_WORDS_PER_VCPU {
        vcpu.eiointc_enable[word].store(u32::MAX as usize, Ordering::Release);
        vcpu.eiointc_bounce[word].store(u32::MAX as usize, Ordering::Release);
    }
    for word in 0..EIOINTC_IPMAP_WORDS {
        vcpu.eiointc_ipmap[word].store(0x0101_0101, Ordering::Release);
    }
    for word in 0..EIOINTC_COREMAP_WORDS {
        vcpu.eiointc_coremap[word].store(0x0101_0101, Ordering::Release);
    }
}

fn iocsr_access_len_from_ty(ty: usize) -> usize {
    match ty {
        0 | 4 => 1,
        1 | 5 => 2,
        2 | 6 => 4,
        3 | 7 => 8,
        _ => 0,
    }
}

pub(crate) fn inject_guest_eiointc_vector(
    state: &LoongArchIocsrState,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    vector: usize,
) -> Option<usize> {
    if vector >= EIOINTC_VECTOR_COUNT {
        return None;
    }

    let vcpu = state.vcpu(vcpu_id)?;
    let reg = vector / 64;
    let bit = vector % 64;
    let new = vcpu.eiointc_isr[reg].fetch_or(1usize << bit, Ordering::AcqRel) | (1usize << bit);
    log_eiointc_trace("inject", vm_id, vcpu_id, reg, vector, new);
    eiointc_cpu_irq_for_source(vcpu, vector).or(Some(EIOINTC_HWI_BASE + reg))
}

fn read_guest_iocsr(
    state: &LoongArchIocsrState,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    addr: usize,
    len: usize,
) -> Option<usize> {
    let vcpu = state.vcpu(vcpu_id)?;
    match addr {
        LOONGARCH_IOCSR_IPI_STATUS => Some(vcpu.ipi_status.load(Ordering::Acquire)),
        LOONGARCH_IOCSR_IPI_EN => Some(vcpu.ipi_enable.load(Ordering::Acquire)),
        LOONGARCH_IOCSR_IPI_SET | LOONGARCH_IOCSR_IPI_CLEAR | LOONGARCH_IOCSR_IPI_SEND => Some(0),
        LOONGARCH_IOCSR_MAIL_BUF0..=LOONGARCH_IOCSR_MAIL_BUF3 => iocsr_mail_buf_index(addr)
            .map(|mail_index| vcpu.mail_buf[mail_index].load(Ordering::Acquire)),
        LOONGARCH_IOCSR_MBUF_SEND => Some(0),
        LOONGARCH_IOCSR_ANY_SEND => Some(0),
        EXTIOI_VIRT_FEATURES => Some(extioi_features_value()),
        EXTIOI_VIRT_CONFIG => Some(vcpu.eiointc_virt_config.load(Ordering::Acquire)),
        EIOINTC_NODEMAP_BASE..EIOINTC_NODEMAP_END => Some(read_atomic_u32_slots(
            &vcpu.eiointc_nodemap,
            addr,
            EIOINTC_NODEMAP_BASE,
            len,
        )),
        EIOINTC_IPMAP_BASE..EIOINTC_IPMAP_END => Some(read_atomic_u32_slots(
            &vcpu.eiointc_ipmap,
            addr,
            EIOINTC_IPMAP_BASE,
            len,
        )),
        EIOINTC_ENABLE_BASE..EIOINTC_ENABLE_END => Some(read_atomic_u32_slots(
            &vcpu.eiointc_enable,
            addr,
            EIOINTC_ENABLE_BASE,
            len,
        )),
        EIOINTC_BOUNCE_BASE..EIOINTC_BOUNCE_END => Some(read_atomic_u32_slots(
            &vcpu.eiointc_bounce,
            addr,
            EIOINTC_BOUNCE_BASE,
            len,
        )),
        EIOINTC_ISR_COMPAT_BASE..EIOINTC_ISR_COMPAT_END => {
            let value = read_eiointc_isr_slots(vcpu, addr, EIOINTC_ISR_COMPAT_BASE, len);
            log_eiointc_trace(
                "read",
                vm_id,
                vcpu_id,
                (addr - EIOINTC_ISR_COMPAT_BASE) / 8,
                addr - EIOINTC_ISR_COMPAT_BASE,
                value,
            );
            Some(value)
        }
        EIOINTC_ISR_BASE..EIOINTC_ISR_END => {
            let value = read_eiointc_isr_slots(vcpu, addr, EIOINTC_ISR_BASE, len);
            log_eiointc_trace(
                "read",
                vm_id,
                vcpu_id,
                (addr - EIOINTC_ISR_BASE) / 8,
                addr - EIOINTC_ISR_BASE,
                value,
            );
            Some(value)
        }
        EIOINTC_COREMAP_BASE..EIOINTC_COREMAP_END => Some(read_atomic_u32_slots(
            &vcpu.eiointc_coremap,
            addr,
            EIOINTC_COREMAP_BASE,
            len,
        )),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct IocsrWrite {
    address: usize,
    width: usize,
    value: usize,
}

fn write_guest_iocsr<H: LoongArchHostOps>(
    cpu_pin: &CpuPin,
    state: &LoongArchIocsrState,
    ctx: &mut LoongArchContextFrame,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    write: IocsrWrite,
) -> Option<LoongArchVmExit> {
    let IocsrWrite {
        address: addr,
        width: len,
        value,
    } = write;
    let vcpu = state.vcpu(vcpu_id)?;
    match addr {
        LOONGARCH_IOCSR_IPI_STATUS => Some(LoongArchVmExit::Nothing),
        LOONGARCH_IOCSR_IPI_EN => {
            vcpu.ipi_enable.store(value, Ordering::Release);
            Some(LoongArchVmExit::Nothing)
        }
        LOONGARCH_IOCSR_IPI_SET => {
            vcpu.ipi_status.fetch_or(value, Ordering::AcqRel);
            ctx.gcsr_estat |= IPI_BIT;
            Some(LoongArchVmExit::Nothing)
        }
        LOONGARCH_IOCSR_IPI_CLEAR => {
            let new_status = vcpu.ipi_status.fetch_and(!value, Ordering::AcqRel) & !value;
            if new_status == 0 {
                ctx.gcsr_estat &= !IPI_BIT;
            }
            Some(LoongArchVmExit::Nothing)
        }
        LOONGARCH_IOCSR_IPI_SEND => {
            let target_cpu = iocsr_send_cpu(value);
            let action = iocsr_send_action(value);
            if let Some(target_vcpu) = state.vcpu(target_cpu) {
                target_vcpu
                    .ipi_status
                    .fetch_or(1usize << action, Ordering::AcqRel);
                if target_cpu == vcpu_id {
                    ctx.gcsr_estat |= IPI_BIT;
                } else {
                    H::inject_interrupt(vm_id, target_cpu, INT_IPI);
                }
            } else {
                log::debug!(
                    "LoongArch guest IOCSR IPI_SEND ignored for unsupported target_cpu={} \
                     action={}",
                    target_cpu,
                    action
                );
            }
            Some(LoongArchVmExit::Nothing)
        }
        LOONGARCH_IOCSR_MAIL_BUF0..=LOONGARCH_IOCSR_MAIL_BUF3 => {
            if let Some(mail_index) = iocsr_mail_buf_index(addr) {
                vcpu.mail_buf[mail_index].store(value, Ordering::Release);
            }
            Some(LoongArchVmExit::Nothing)
        }
        LOONGARCH_IOCSR_MBUF_SEND => {
            let target_cpu = iocsr_mbuf_send_cpu(value);
            let mail_word = iocsr_mbuf_send_box(value);
            let mail_buf = mail_word / 2;
            let is_high_word = mail_word % 2 == 1;
            if mail_buf < IOCSR_MAIL_BUF_COUNT {
                if let Some(target_vcpu) = state.vcpu(target_cpu) {
                    let old = target_vcpu.mail_buf[mail_buf].load(Ordering::Acquire);
                    let new = if is_high_word {
                        (old & !IOCSR_MBUF_SEND_H32_MASK)
                            | (iocsr_mbuf_send_buf(value) << u32::BITS)
                    } else {
                        let low = iocsr_mbuf_send_buf(value);
                        (old & IOCSR_MBUF_SEND_H32_MASK) | low
                    };
                    target_vcpu.mail_buf[mail_buf].store(new, Ordering::Release);
                } else {
                    log::debug!(
                        "LoongArch guest IOCSR MBUF_SEND ignored for unsupported target_cpu={} \
                         mail_buf={}",
                        target_cpu,
                        mail_buf
                    );
                }
            }
            if target_cpu == vcpu_id {
                ctx.gcsr_estat |= IPI_BIT;
            }
            Some(LoongArchVmExit::Nothing)
        }
        LOONGARCH_IOCSR_ANY_SEND => {
            let target_cpu = iocsr_send_cpu(value);
            let target = value & 0xffff;
            if target_cpu == vcpu_id {
                write_guest_iocsr_send_data::<H>(
                    cpu_pin, state, ctx, vm_id, vcpu_id, target, value,
                );
            }
            Some(LoongArchVmExit::Nothing)
        }
        EXTIOI_VIRT_CONFIG => {
            vcpu.eiointc_virt_config.store(value, Ordering::Release);
            update_guest_eiointc_irq(cpu_pin, ctx, vcpu);
            Some(LoongArchVmExit::Nothing)
        }
        EIOINTC_NODEMAP_BASE..EIOINTC_NODEMAP_END => {
            write_atomic_u32_slots(
                &vcpu.eiointc_nodemap,
                addr,
                EIOINTC_NODEMAP_BASE,
                len,
                value,
            );
            Some(LoongArchVmExit::Nothing)
        }
        EIOINTC_IPMAP_BASE..EIOINTC_IPMAP_END => {
            write_atomic_u32_slots(&vcpu.eiointc_ipmap, addr, EIOINTC_IPMAP_BASE, len, value);
            update_guest_eiointc_irq(cpu_pin, ctx, vcpu);
            Some(LoongArchVmExit::Nothing)
        }
        EIOINTC_ENABLE_BASE..EIOINTC_ENABLE_END => {
            write_atomic_u32_slots(&vcpu.eiointc_enable, addr, EIOINTC_ENABLE_BASE, len, value);
            update_guest_eiointc_irq(cpu_pin, ctx, vcpu);
            Some(LoongArchVmExit::Nothing)
        }
        EIOINTC_BOUNCE_BASE..EIOINTC_BOUNCE_END => {
            write_atomic_u32_slots(&vcpu.eiointc_bounce, addr, EIOINTC_BOUNCE_BASE, len, value);
            Some(LoongArchVmExit::Nothing)
        }
        EIOINTC_ISR_COMPAT_BASE..EIOINTC_ISR_COMPAT_END => {
            clear_eiointc_isr_slots(vcpu, addr, EIOINTC_ISR_COMPAT_BASE, len, value);
            update_guest_eiointc_irq(cpu_pin, ctx, vcpu);
            Some(LoongArchVmExit::Nothing)
        }
        EIOINTC_ISR_BASE..EIOINTC_ISR_END if is_eiointc_isr_addr(addr) => {
            clear_eiointc_isr_slots(vcpu, addr, EIOINTC_ISR_BASE, len, value);
            update_guest_eiointc_irq(cpu_pin, ctx, vcpu);
            log_eiointc_trace(
                "clear",
                vm_id,
                vcpu_id,
                (addr - EIOINTC_ISR_BASE) / 8,
                addr - EIOINTC_ISR_BASE,
                read_eiointc_isr_slots(vcpu, addr, EIOINTC_ISR_BASE, len),
            );
            Some(LoongArchVmExit::Nothing)
        }
        EIOINTC_COREMAP_BASE..EIOINTC_COREMAP_END => {
            write_atomic_u32_slots(
                &vcpu.eiointc_coremap,
                addr,
                EIOINTC_COREMAP_BASE,
                len,
                value,
            );
            update_guest_eiointc_irq(cpu_pin, ctx, vcpu);
            Some(LoongArchVmExit::Nothing)
        }
        EIOINTC_GUEST_OWNED_BASE..EIOINTC_GUEST_OWNED_END => Some(LoongArchVmExit::Nothing),
        _ => None,
    }
}

fn write_guest_iocsr_send_data<H: LoongArchHostOps>(
    cpu_pin: &CpuPin,
    state: &LoongArchIocsrState,
    ctx: &mut LoongArchContextFrame,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    target: usize,
    value: usize,
) {
    let preserve_mask = iocsr_send_byte_mask(value);
    let mut data = iocsr_send_data(value);

    if preserve_mask != 0 {
        let old = read_guest_iocsr(state, vm_id, vcpu_id, target, 4).unwrap_or(0) as u32;
        let mut byte_mask = 0u32;
        for byte in 0..4 {
            if preserve_mask & (1 << byte) != 0 {
                byte_mask |= 0xff << (byte * 8);
            }
        }
        data = ((old & byte_mask) as usize) | (data & !(byte_mask as usize));
    }

    let _ = write_guest_iocsr::<H>(
        cpu_pin,
        state,
        ctx,
        vm_id,
        vcpu_id,
        IocsrWrite {
            address: target,
            width: 4,
            value: data,
        },
    );
}

pub(crate) fn inject_enabled_pending_interrupt(
    cpu_pin: &CpuPin,
    state: &LoongArchIocsrState,
    ctx: &mut LoongArchContextFrame,
    vcpu_id: LoongArchVcpuId,
) -> bool {
    if let Some(vcpu) = state.vcpu(vcpu_id) {
        update_guest_eiointc_irq(cpu_pin, ctx, vcpu);
    }
    let pending_enabled = ctx.gcsr_estat & ctx.gcsr_ectl & LOCAL_INTERRUPT_MASK;
    if ctx.gcsr_eentry != 0
        && ctx.gcsr_crmd & crate::registers::crmd_interrupt_enable_value() != 0
        && let Some(vector) = decode_interrupt_vector(pending_enabled)
    {
        inject_guest_interrupt_at(ctx, vector, get_guest_pc(ctx));
        true
    } else {
        false
    }
}

pub(crate) fn emulate_iocsr<H: LoongArchHostOps>(
    cpu_pin: &CpuPin,
    state: &LoongArchIocsrState,
    ctx: &mut LoongArchContextFrame,
    ins: usize,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
) -> LoongArchVmExit {
    let ty = extract_field(ins, 10, 3);
    let rd = extract_field(ins, 0, 5);
    let rj = extract_field(ins, 5, 5);
    let rj_value = ctx.x[rj];
    let len = iocsr_access_len_from_ty(ty);
    let target_log = |ctx: &LoongArchContextFrame, op: &str, value: usize| {
        if (ins == 0x0648_0f78 || get_guest_pc(ctx) == 0x9000_0000_00b9_7fe8)
            && TARGET_IOCSR_LOGS.fetch_add(1, Ordering::Relaxed) < 128
        {
            log::trace!(
                "LoongArch target IOCSR {}: pc={:#x}, ins={:#x}, ty={}, rd={}, rj={}, addr={:#x}, \
                 value={:#x}, a0={:#x}, a1={:#x}, a2={:#x}, s1={:#x}, s4={:#x}, crmd={:#x}, \
                 estat={:#x}, ecfg={:#x}",
                op,
                get_guest_pc(ctx),
                ins,
                ty,
                rd,
                rj,
                rj_value,
                value,
                ctx.get_a0(),
                ctx.get_a1(),
                ctx.get_a2(),
                ctx.x[23],
                ctx.x[20],
                ctx.gcsr_crmd,
                ctx.gcsr_estat,
                ctx.gcsr_ectl,
            );
        }
    };
    let log_iocsr = |ctx: &LoongArchContextFrame, op: &str, value: usize| {
        if IOCSR_EXIT_LOGS.fetch_add(1, Ordering::Relaxed) < 64 {
            log::trace!(
                "LoongArch IOCSR {}: pc={:#x}, ins={:#x}, ty={}, rd={}, rj={}, addr={:#x}, \
                 value={:#x}",
                op,
                get_guest_pc(ctx),
                ins,
                ty,
                rd,
                rj,
                rj_value,
                value
            );
        }
    };

    match ty {
        0 => {
            let value = read_guest_iocsr(state, vm_id, vcpu_id, rj_value, len)
                .unwrap_or_else(|| host_iocsr_read_b(rj_value));
            log_iocsr(ctx, "read.b", value);
            target_log(ctx, "read.b", value);
            ctx.set_gpr(rd, (value as i8) as isize as usize);
        }
        1 => {
            let value = read_guest_iocsr(state, vm_id, vcpu_id, rj_value, len)
                .unwrap_or_else(|| host_iocsr_read_h(rj_value));
            log_iocsr(ctx, "read.h", value);
            target_log(ctx, "read.h", value);
            ctx.set_gpr(rd, (value as i16) as isize as usize);
        }
        2 => {
            let value = read_guest_iocsr(state, vm_id, vcpu_id, rj_value, len)
                .unwrap_or_else(|| host_iocsr_read_w(rj_value));
            log_iocsr(ctx, "read.w", value);
            target_log(ctx, "read.w", value);
            ctx.set_gpr(rd, (value as i32) as isize as usize);
        }
        3 => {
            let value = read_guest_iocsr(state, vm_id, vcpu_id, rj_value, len)
                .unwrap_or_else(|| host_iocsr_read_d(rj_value));
            log_iocsr(ctx, "read.d", value);
            target_log(ctx, "read.d", value);
            ctx.set_gpr(rd, value);
        }
        4 => {
            if let Some(reason) = write_guest_iocsr::<H>(
                cpu_pin,
                state,
                ctx,
                vm_id,
                vcpu_id,
                IocsrWrite {
                    address: rj_value,
                    width: len,
                    value: ctx.x[rd] as u8 as usize,
                },
            ) {
                advance_guest_pc(ctx);
                return reason;
            } else {
                log_iocsr(ctx, "write.b", ctx.x[rd] as u8 as usize);
                host_iocsr_write_b(rj_value, ctx.x[rd]);
            }
        }
        5 => {
            if let Some(reason) = write_guest_iocsr::<H>(
                cpu_pin,
                state,
                ctx,
                vm_id,
                vcpu_id,
                IocsrWrite {
                    address: rj_value,
                    width: len,
                    value: ctx.x[rd] as u16 as usize,
                },
            ) {
                advance_guest_pc(ctx);
                return reason;
            } else {
                log_iocsr(ctx, "write.h", ctx.x[rd] as u16 as usize);
                host_iocsr_write_h(rj_value, ctx.x[rd]);
            }
        }
        6 => {
            if let Some(reason) = write_guest_iocsr::<H>(
                cpu_pin,
                state,
                ctx,
                vm_id,
                vcpu_id,
                IocsrWrite {
                    address: rj_value,
                    width: len,
                    value: ctx.x[rd] as u32 as usize,
                },
            ) {
                advance_guest_pc(ctx);
                return reason;
            } else {
                log_iocsr(ctx, "write.w", ctx.x[rd] as u32 as usize);
                host_iocsr_write_w(rj_value, ctx.x[rd]);
            }
        }
        7 => {
            if let Some(reason) = write_guest_iocsr::<H>(
                cpu_pin,
                state,
                ctx,
                vm_id,
                vcpu_id,
                IocsrWrite {
                    address: rj_value,
                    width: len,
                    value: ctx.x[rd],
                },
            ) {
                advance_guest_pc(ctx);
                return reason;
            } else {
                let is_eiointc_complete = is_eiointc_isr_addr(rj_value);
                log_iocsr(ctx, "write.d", ctx.x[rd]);
                host_iocsr_write_d(rj_value, ctx.x[rd]);
                if is_eiointc_complete && !host_eiointc_has_pending() {
                    ctx.gcsr_estat &= !HWI_MASK;
                }
            }
        }
        _ => panic!("invalid LoongArch IOCSR opcode type: {ty}"),
    }

    advance_guest_pc(ctx);
    LoongArchVmExit::Nothing
}

fn log_eiointc_trace(
    op: &str,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    reg: usize,
    vector_hint: usize,
    value: usize,
) {
    if EIOINTC_TRACE_LOGS.fetch_add(1, Ordering::Relaxed) < 128 {
        log::trace!(
            "LoongArch guest EIOINTC {op}: VM[{vm_id}] VCpu[{vcpu_id}] reg={}, vector_hint={}, \
             isr={:#x}",
            reg,
            vector_hint,
            value
        );
    }
}

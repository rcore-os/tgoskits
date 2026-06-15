use alloc::boxed::Box;
use core::{
    mem::offset_of,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ax_errno::AxResult;
use axvcpu::{AxVCpuExitReason, GuestPhysAddr, MappingFlags, VCpuId, VMId};

use crate::{context_frame::LoongArchContextFrame, host};

const ECODE_HVC: usize = 0x17;
const ECODE_GSPR: usize = 0x16;
const ECODE_PIL: usize = 0x1;
const ECODE_PIS: usize = 0x2;
const ECODE_PIF: usize = 0x3;
const ECODE_PME: usize = 0x4;
const ECODE_PNR: usize = 0x5;
const ECODE_PNX: usize = 0x6;
const ECODE_PPI: usize = 0x7;
const ECODE_ADE: usize = 0x8;
const ESUBCODE_ADEF: usize = 0x0;
const ESUBCODE_ADEM: usize = 0x1;
const ECODE_RSE: usize = 0x10;
const LOCAL_INTERRUPT_MASK: usize = (1 << 13) - 1;
const INT_HWI0: usize = 2;
const INT_HWI7: usize = 9;
const INT_TIMER: usize = 11;
const INT_IPI: usize = 12;
const TIMER_BIT: usize = 1 << INT_TIMER;
const IPI_BIT: usize = 1 << INT_IPI;
const HWI_MASK: usize = ((1 << (INT_HWI7 + 1)) - 1) & !((1 << INT_HWI0) - 1);
const CPUCFG2_CRYPTO: usize = 1 << 9;
const CSR_CRMD: usize = 0x0;
const CSR_PRMD: usize = 0x1;
const CSR_EUEN: usize = 0x2;
const CSR_MISC: usize = 0x3;
const CSR_ECFG: usize = 0x4;
const CSR_ESTAT: usize = 0x5;
const CSR_ERA: usize = 0x6;
const CSR_BADV: usize = 0x7;
const CSR_BADI: usize = 0x8;
const CSR_BADI_U16: u16 = 0x8;
const CSR_EENTRY: usize = 0xc;
const CSR_TLBIDX: usize = 0x10;
const CSR_TLBEHI: usize = 0x11;
const CSR_TLBELO0: usize = 0x12;
const CSR_TLBELO1: usize = 0x13;
const CSR_ASID: usize = 0x18;
const CSR_PGDL: usize = 0x19;
const CSR_PGDH: usize = 0x1a;
const CSR_PGD: usize = 0x1b;
const CSR_PWCL: usize = 0x1c;
const CSR_PWCH: usize = 0x1d;
const CSR_STLBPS: usize = 0x1e;
const CSR_RAVCFG: usize = 0x1f;
const CSR_CPUID: usize = 0x20;
const CSR_PRCFG1: usize = 0x21;
const CSR_PRCFG2: usize = 0x22;
const CSR_PRCFG3: usize = 0x23;
const CSR_TID: usize = 0x40;
const CSR_LLBCTL: usize = 0x60;
const CSR_TLBRENTRY: usize = 0x88;
const CSR_TLBRBADV: usize = 0x89;
const CSR_TLBRERA: usize = 0x8a;
const CSR_TLBRSAVE: usize = 0x8b;
const CSR_TLBRELO0: usize = 0x8c;
const CSR_TLBRELO1: usize = 0x8d;
const CSR_TLBREHI: usize = 0x8e;
const CSR_TLBRPRMD: usize = 0x8f;
const CSR_DMW0: usize = 0x180;
const CSR_DMW1: usize = 0x181;
const CSR_DMW2: usize = 0x182;
const CSR_DMW3: usize = 0x183;
const CSR_TICLR_TI: usize = 0x1;
const CSR_TCFG_EN: usize = 1 << 0;
const CSR_TCFG_PERIODIC: usize = 1 << 1;
const CSR_TCFG_INITVAL_MASK: usize = !0x3;
const CSR_CRMD_PLV_MASK: usize = 0b11;
const CSR_CRMD_IE: usize = 1 << 2;
const CSR_CRMD_DA: usize = 1 << 3;
const CSR_CRMD_PG: usize = 1 << 4;
const CSR_ESTAT_EXC_MASK: usize = (0x3f << 16) | (0x1ff << 22);
const CSR_ECFG_VS_SHIFT: usize = 16;
const CSR_ECFG_VS_MASK: usize = 0b111 << CSR_ECFG_VS_SHIFT;
const CSR_TLBRERA_ISTLBR: usize = 1;
const CSR_TLBREHI_PS_MASK: usize = 0x3f;
const CSR_TLBREHI_VPPN_MASK: usize = 0x0000_ffff_ffff_e000;
const DEFAULT_TLB_PAGE_SHIFT: usize = 12;
const GUEST_RAM_START: usize = 0x0008_0000;
const GUEST_HIGH_RAM_START: usize = 0x8000_0000;
const GUEST_HIGH_RAM_END: usize = 0xb000_0000;
const QEMU_VIRT_MMIO_START: usize = 0x1000_0000;
const QEMU_VIRT_MMIO_END: usize = 0x8000_0000;
const GUEST_RAM_END: usize = QEMU_VIRT_MMIO_START;
const EIOINTC_ISR_BASE: usize = 0x1800;
const EIOINTC_ISR_REG_COUNT: usize = 4;
const EIOINTC_ISR_END: usize = EIOINTC_ISR_BASE + EIOINTC_ISR_REG_COUNT * 8;
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
const MAX_IOCSR_VMS: usize = 16;
const MAX_IOCSR_CPUS: usize = 16;
const LOONGARCH_IOCSR_IPI_STATUS: usize = 0x1000;
const LOONGARCH_IOCSR_IPI_EN: usize = 0x1004;
const LOONGARCH_IOCSR_IPI_SET: usize = 0x1008;
const LOONGARCH_IOCSR_IPI_CLEAR: usize = 0x100c;
const LOONGARCH_IOCSR_MAIL_BUF0: usize = 0x1020;
const LOONGARCH_IOCSR_MAIL_BUF3: usize = 0x1038;
const LOONGARCH_IOCSR_IPI_SEND: usize = 0x1040;
const LOONGARCH_IOCSR_MBUF_SEND: usize = 0x1048;
const LOONGARCH_IOCSR_ANY_SEND: usize = 0x1158;
const LOONGARCH_IOCSR_ANY_SEND_BUF_SHIFT: usize = 32;
const IOCSR_SEND_CPU_SHIFT: usize = 16;
const IOCSR_SEND_CPU_MASK: usize = 0x3ff;
const IOCSR_SEND_BYTE_MASK_SHIFT: usize = 27;
const IOCSR_IPI_ACTION_MASK: usize = 0x1f;
const IOCSR_MBUF_SEND_BOX_SHIFT: usize = 2;
const IOCSR_MBUF_SEND_BUF_SHIFT: usize = 32;
const IOCSR_MBUF_SEND_H32_MASK: usize = 0xffff_ffff_0000_0000;
const IOCSR_MAIL_BUF_COUNT: usize = 4;
const EIOINTC_NODEMAP_WORDS: usize = 8;
const EIOINTC_IPMAP_WORDS: usize = 2;
const EIOINTC_ENABLE_WORDS_PER_VCPU: usize = 8;
const EIOINTC_BOUNCE_WORDS: usize = 8;
const EIOINTC_COREMAP_WORDS: usize = 64;
const EXTIOI_VIRT_FEATURES: usize = 0x4000_0000;
const EXTIOI_VIRT_CONFIG: usize = 0x4000_0004;
const EXTIOI_HAS_VIRT_EXTENSION: usize = 1 << 0;
const EXTIOI_HAS_ENABLE_OPTION: usize = 1 << 1;
const EXTIOI_HAS_INT_ENCODE: usize = 1 << 2;
const EXTIOI_HAS_CPU_ENCODE: usize = 1 << 3;
const EXTIOI_ENABLE_CPU_ENCODE: usize = 1 << 3;

static IOCSR_IPI_STATUS_WORDS: [AtomicUsize; MAX_IOCSR_VMS * MAX_IOCSR_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_IOCSR_VMS * MAX_IOCSR_CPUS];
static IOCSR_IPI_ENABLE_WORDS: [AtomicUsize; MAX_IOCSR_VMS * MAX_IOCSR_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_IOCSR_VMS * MAX_IOCSR_CPUS];
static IOCSR_MAIL_BUF_WORDS: [AtomicUsize; MAX_IOCSR_VMS * MAX_IOCSR_CPUS * IOCSR_MAIL_BUF_COUNT] =
    [const { AtomicUsize::new(0) }; MAX_IOCSR_VMS * MAX_IOCSR_CPUS * IOCSR_MAIL_BUF_COUNT];
static EIOINTC_ISR_WORDS: [AtomicUsize; MAX_IOCSR_VMS * MAX_IOCSR_CPUS * EIOINTC_ISR_REG_COUNT] =
    [const { AtomicUsize::new(0) }; MAX_IOCSR_VMS * MAX_IOCSR_CPUS * EIOINTC_ISR_REG_COUNT];
static EIOINTC_NODEMAP_WORDS_STORAGE: [AtomicUsize;
    MAX_IOCSR_VMS * MAX_IOCSR_CPUS * EIOINTC_NODEMAP_WORDS] =
    [const { AtomicUsize::new(0) }; MAX_IOCSR_VMS * MAX_IOCSR_CPUS * EIOINTC_NODEMAP_WORDS];
static EIOINTC_IPMAP_WORDS_STORAGE: [AtomicUsize;
    MAX_IOCSR_VMS * MAX_IOCSR_CPUS * EIOINTC_IPMAP_WORDS] =
    [const { AtomicUsize::new(0) }; MAX_IOCSR_VMS * MAX_IOCSR_CPUS * EIOINTC_IPMAP_WORDS];
static EIOINTC_ENABLE_WORDS: [AtomicUsize;
    MAX_IOCSR_VMS * MAX_IOCSR_CPUS * EIOINTC_ENABLE_WORDS_PER_VCPU] =
    [const { AtomicUsize::new(0) }; MAX_IOCSR_VMS * MAX_IOCSR_CPUS * EIOINTC_ENABLE_WORDS_PER_VCPU];
static EIOINTC_BOUNCE_WORDS_STORAGE: [AtomicUsize;
    MAX_IOCSR_VMS * MAX_IOCSR_CPUS * EIOINTC_BOUNCE_WORDS] =
    [const { AtomicUsize::new(0) }; MAX_IOCSR_VMS * MAX_IOCSR_CPUS * EIOINTC_BOUNCE_WORDS];
static EIOINTC_COREMAP_WORDS_STORAGE: [AtomicUsize;
    MAX_IOCSR_VMS * MAX_IOCSR_CPUS * EIOINTC_COREMAP_WORDS] =
    [const { AtomicUsize::new(0) }; MAX_IOCSR_VMS * MAX_IOCSR_CPUS * EIOINTC_COREMAP_WORDS];
static EIOINTC_VIRT_CONFIG_WORDS: [AtomicUsize; MAX_IOCSR_VMS * MAX_IOCSR_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_IOCSR_VMS * MAX_IOCSR_CPUS];
static NESTED_FAULT_LOGS: AtomicUsize = AtomicUsize::new(0);
static SYNC_EXIT_LOGS: AtomicUsize = AtomicUsize::new(0);
static IOCSR_EXIT_LOGS: AtomicUsize = AtomicUsize::new(0);
static EIOINTC_TRACE_LOGS: AtomicUsize = AtomicUsize::new(0);
static TARGET_GSPR_LOGS: AtomicUsize = AtomicUsize::new(0);
static TARGET_IOCSR_LOGS: AtomicUsize = AtomicUsize::new(0);
static IDLE_EXIT_LOGS: AtomicUsize = AtomicUsize::new(0);
static GUEST_TIMER_LOGS: AtomicUsize = AtomicUsize::new(0);

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapKind {
    Synchronous = 0,
    Irq         = 1,
}

impl TryFrom<u8> for TrapKind {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Synchronous),
            1 => Ok(Self::Irq),
            _ => Err(()),
        }
    }
}

fn get_exception_code(ctx: &LoongArchContextFrame) -> usize {
    (ctx.host_estat >> 16) & 0x3f
}

fn get_exception_subcode(ctx: &LoongArchContextFrame) -> usize {
    (ctx.host_estat >> 22) & 0x1ff
}

fn is_host_tlb_refill(ctx: &LoongArchContextFrame) -> bool {
    ctx.host_tlbrera & 0x1 != 0
}

fn get_guest_pc(ctx: &LoongArchContextFrame) -> usize {
    ctx.guest_exception_pc()
}

fn direct_map_guest_addr_to_gpa(addr: usize) -> usize {
    if matches!(addr >> 48, 0x8000 | 0x9000 | 0xa000) {
        addr & 0x0000_ffff_ffff_ffff
    } else if matches!(addr >> 44, 0x8..=0xa) {
        addr & 0x0000_0fff_ffff_ffff
    } else if (0xffff_8000_0000..0xffff_c000_0000).contains(&addr) {
        let gpa = addr - 0xffff_8000_0000;
        if is_known_guest_physical_addr(gpa) {
            gpa
        } else {
            addr
        }
    } else {
        addr
    }
}

fn get_refill_access_flags(ctx: &LoongArchContextFrame) -> MappingFlags {
    let badv = direct_map_guest_addr_to_gpa(get_badv(ctx));
    let pc_gpa = direct_map_guest_addr_to_gpa(get_guest_pc(ctx));
    if badv == pc_gpa {
        MappingFlags::EXECUTE
    } else {
        MappingFlags::READ | MappingFlags::WRITE
    }
}

fn guest_paging_enabled(ctx: &LoongArchContextFrame) -> bool {
    ctx.gcsr_crmd & CSR_CRMD_PG != 0
}

fn is_guest_direct_mapped_va(addr: usize) -> bool {
    matches!(addr >> 48, 0x8000 | 0x9000 | 0xa000) || matches!(addr >> 44, 0x8..=0xa)
}

fn is_known_guest_physical_addr(addr: usize) -> bool {
    (GUEST_RAM_START..GUEST_RAM_END).contains(&addr)
        || (GUEST_HIGH_RAM_START..GUEST_HIGH_RAM_END).contains(&addr)
        || (QEMU_VIRT_MMIO_START..QEMU_VIRT_MMIO_END).contains(&addr)
}

fn is_eiointc_isr_addr(addr: usize) -> bool {
    (EIOINTC_ISR_BASE..EIOINTC_ISR_END).contains(&addr) && addr.is_multiple_of(8)
}

fn host_eiointc_has_pending() -> bool {
    for reg in 0..EIOINTC_ISR_REG_COUNT {
        let addr = EIOINTC_ISR_BASE + reg * 8;
        let value: usize;
        unsafe {
            core::arch::asm!("iocsrrd.d {}, {}", out(reg) value, in(reg) addr);
        }
        if value != 0 {
            return true;
        }
    }

    false
}

fn iocsr_vcpu_index(vm_id: VMId, vcpu_id: VCpuId) -> Option<usize> {
    if vm_id < MAX_IOCSR_VMS && vcpu_id < MAX_IOCSR_CPUS {
        Some(vm_id * MAX_IOCSR_CPUS + vcpu_id)
    } else {
        None
    }
}

fn iocsr_mail_buf_index(vcpu_index: usize, addr: usize) -> Option<usize> {
    if !(LOONGARCH_IOCSR_MAIL_BUF0..=LOONGARCH_IOCSR_MAIL_BUF3).contains(&addr)
        || !(addr - LOONGARCH_IOCSR_MAIL_BUF0).is_multiple_of(8)
    {
        return None;
    }

    Some(vcpu_index * IOCSR_MAIL_BUF_COUNT + (addr - LOONGARCH_IOCSR_MAIL_BUF0) / 8)
}

fn eiointc_isr_index(vcpu_index: usize, reg: usize) -> usize {
    vcpu_index * EIOINTC_ISR_REG_COUNT + reg
}

fn eiointc_word_index(vcpu_index: usize, words_per_vcpu: usize, word: usize) -> usize {
    vcpu_index * words_per_vcpu + word
}

fn read_atomic_u32_slots(
    slots: &[AtomicUsize],
    vcpu_index: usize,
    words_per_vcpu: usize,
    addr: usize,
    base: usize,
    len: usize,
) -> usize {
    let word = (addr - base) >> 2;
    let value = slots
        .get(eiointc_word_index(vcpu_index, words_per_vcpu, word))
        .map(|slot| slot.load(Ordering::Acquire) as u32 as usize)
        .unwrap_or(0);
    if len == 8 {
        value
            | (slots
                .get(eiointc_word_index(vcpu_index, words_per_vcpu, word + 1))
                .map(|slot| (slot.load(Ordering::Acquire) as u32 as usize) << 32)
                .unwrap_or(0))
    } else {
        value
    }
}

fn write_atomic_u32_slots(
    slots: &[AtomicUsize],
    vcpu_index: usize,
    words_per_vcpu: usize,
    addr: usize,
    base: usize,
    len: usize,
    value: usize,
) {
    let word = (addr - base) >> 2;
    if let Some(slot) = slots.get(eiointc_word_index(vcpu_index, words_per_vcpu, word)) {
        slot.store(value as u32 as usize, Ordering::Release);
    }
    if len == 8
        && let Some(slot) = slots.get(eiointc_word_index(vcpu_index, words_per_vcpu, word + 1))
    {
        slot.store((value >> 32) as u32 as usize, Ordering::Release);
    }
}

fn eiointc_isr_word(vcpu_index: usize, word: usize) -> usize {
    EIOINTC_ISR_WORDS
        .get(eiointc_isr_index(vcpu_index, word / 2))
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

fn clear_eiointc_isr_word(vcpu_index: usize, word: usize, value: usize) {
    if let Some(slot) = EIOINTC_ISR_WORDS.get(eiointc_isr_index(vcpu_index, word / 2)) {
        let shift = (word % 2) * 32;
        let mask = (value as u32 as usize) << shift;
        slot.fetch_and(!mask, Ordering::AcqRel);
    }
}

fn read_eiointc_isr_slots(vcpu_index: usize, addr: usize, base: usize, len: usize) -> usize {
    let word = (addr - base) >> 2;
    let value = eiointc_isr_word(vcpu_index, word);
    if len == 8 {
        value | (eiointc_isr_word(vcpu_index, word + 1) << 32)
    } else {
        value
    }
}

fn clear_eiointc_isr_slots(vcpu_index: usize, addr: usize, base: usize, len: usize, value: usize) {
    let word = (addr - base) >> 2;
    clear_eiointc_isr_word(vcpu_index, word, value);
    if len == 8 {
        clear_eiointc_isr_word(vcpu_index, word + 1, value >> 32);
    }
}

fn eiointc_enable_word(vcpu_index: usize, word: usize) -> usize {
    EIOINTC_ENABLE_WORDS[eiointc_word_index(vcpu_index, EIOINTC_ENABLE_WORDS_PER_VCPU, word)]
        .load(Ordering::Acquire)
}

fn eiointc_ipmap_word(vcpu_index: usize, word: usize) -> usize {
    EIOINTC_IPMAP_WORDS_STORAGE[eiointc_word_index(vcpu_index, EIOINTC_IPMAP_WORDS, word)]
        .load(Ordering::Acquire)
}

fn eiointc_coremap_word(vcpu_index: usize, word: usize) -> usize {
    EIOINTC_COREMAP_WORDS_STORAGE[eiointc_word_index(vcpu_index, EIOINTC_COREMAP_WORDS, word)]
        .load(Ordering::Acquire)
}

fn eiointc_virt_config(vcpu_index: usize) -> usize {
    EIOINTC_VIRT_CONFIG_WORDS[vcpu_index].load(Ordering::Acquire)
}

fn eiointc_group_for_source(vcpu_index: usize, source: usize) -> Option<usize> {
    let ipmap_index = source >> 7;
    let ipmap_byte = (source >> 5) & 0x3;
    let ipmap = eiointc_ipmap_word(vcpu_index, ipmap_index);
    let pin_mask = (ipmap >> (ipmap_byte * 8)) & 0xff;
    if pin_mask == 0 {
        None
    } else {
        Some(pin_mask.trailing_zeros() as usize)
    }
}

fn eiointc_source_targets_vcpu0(vcpu_index: usize, source: usize) -> bool {
    let word = source >> 2;
    let byte = source & 0x3;
    let route = (eiointc_coremap_word(vcpu_index, word) >> (byte * 8)) & 0xff;

    if eiointc_virt_config(vcpu_index) & EXTIOI_ENABLE_CPU_ENCODE != 0 {
        route == 0 || route == 1
    } else {
        route & 0x1 != 0
    }
}

fn eiointc_cpu_irq_for_source(vcpu_index: usize, source: usize) -> Option<usize> {
    let pin = eiointc_group_for_source(vcpu_index, source)?;
    if !eiointc_source_targets_vcpu0(vcpu_index, source) {
        return None;
    }

    if pin <= INT_HWI7 - INT_HWI0 {
        Some(INT_HWI0 + pin)
    } else {
        None
    }
}

fn guest_eiointc_pending_hwi(vcpu_index: usize) -> Option<usize> {
    for word in 0..EIOINTC_ENABLE_WORDS_PER_VCPU {
        let pending = eiointc_isr_word(vcpu_index, word) & eiointc_enable_word(vcpu_index, word);
        if pending == 0 {
            continue;
        }
        for bit in 0..u32::BITS as usize {
            if pending & (1usize << bit) != 0 {
                let source = word * u32::BITS as usize + bit;
                if let Some(hwi) = eiointc_cpu_irq_for_source(vcpu_index, source) {
                    return Some(hwi);
                }
            }
        }
    }
    None
}

fn update_guest_eiointc_irq(ctx: &mut LoongArchContextFrame, vcpu_index: usize) {
    let hwi_bits = HWI_MASK;
    if let Some(hwi) = guest_eiointc_pending_hwi(vcpu_index) {
        ctx.gcsr_estat = (ctx.gcsr_estat & !hwi_bits) | (1usize << hwi);
        crate::registers::set_hwi_interrupts(1usize << hwi);
    } else {
        ctx.gcsr_estat &= !hwi_bits;
        crate::registers::set_hwi_interrupts(0);
    }
}

pub(crate) fn init_guest_iocsr(vm_id: VMId, vcpu_id: VCpuId) {
    let Some(vcpu_index) = iocsr_vcpu_index(vm_id, vcpu_id) else {
        return;
    };

    for word in 0..EIOINTC_NODEMAP_WORDS {
        let value = ((1usize << (word * 2 + 1)) << 16) | (1usize << (word * 2));
        EIOINTC_NODEMAP_WORDS_STORAGE[eiointc_word_index(vcpu_index, EIOINTC_NODEMAP_WORDS, word)]
            .store(value as u32 as usize, Ordering::Release);
    }
    for word in 0..EIOINTC_ENABLE_WORDS_PER_VCPU {
        EIOINTC_ENABLE_WORDS[eiointc_word_index(vcpu_index, EIOINTC_ENABLE_WORDS_PER_VCPU, word)]
            .store(u32::MAX as usize, Ordering::Release);
        EIOINTC_BOUNCE_WORDS_STORAGE[eiointc_word_index(vcpu_index, EIOINTC_BOUNCE_WORDS, word)]
            .store(u32::MAX as usize, Ordering::Release);
    }
    for word in 0..EIOINTC_IPMAP_WORDS {
        EIOINTC_IPMAP_WORDS_STORAGE[eiointc_word_index(vcpu_index, EIOINTC_IPMAP_WORDS, word)]
            .store(0x0101_0101, Ordering::Release);
    }
    for word in 0..EIOINTC_COREMAP_WORDS {
        EIOINTC_COREMAP_WORDS_STORAGE[eiointc_word_index(vcpu_index, EIOINTC_COREMAP_WORDS, word)]
            .store(0x0101_0101, Ordering::Release);
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
    vm_id: VMId,
    vcpu_id: VCpuId,
    vector: usize,
) -> Option<usize> {
    if vector >= EIOINTC_VECTOR_COUNT {
        return None;
    }

    let vcpu_index = iocsr_vcpu_index(vm_id, vcpu_id)?;
    let reg = vector / 64;
    let bit = vector % 64;
    let new = EIOINTC_ISR_WORDS[eiointc_isr_index(vcpu_index, reg)]
        .fetch_or(1usize << bit, Ordering::AcqRel)
        | (1usize << bit);
    log_eiointc_trace("inject", vm_id, vcpu_id, reg, vector, new);
    eiointc_cpu_irq_for_source(vcpu_index, vector).or(Some(EIOINTC_HWI_BASE + reg))
}

fn read_guest_iocsr(vm_id: VMId, vcpu_id: VCpuId, addr: usize, len: usize) -> Option<usize> {
    let index = iocsr_vcpu_index(vm_id, vcpu_id)?;
    match addr {
        LOONGARCH_IOCSR_IPI_STATUS => Some(IOCSR_IPI_STATUS_WORDS[index].load(Ordering::Acquire)),
        LOONGARCH_IOCSR_IPI_EN => Some(IOCSR_IPI_ENABLE_WORDS[index].load(Ordering::Acquire)),
        LOONGARCH_IOCSR_IPI_SET | LOONGARCH_IOCSR_IPI_CLEAR | LOONGARCH_IOCSR_IPI_SEND => Some(0),
        LOONGARCH_IOCSR_MAIL_BUF0..=LOONGARCH_IOCSR_MAIL_BUF3 => iocsr_mail_buf_index(index, addr)
            .map(|mail_index| IOCSR_MAIL_BUF_WORDS[mail_index].load(Ordering::Acquire)),
        LOONGARCH_IOCSR_MBUF_SEND => Some(0),
        LOONGARCH_IOCSR_ANY_SEND => Some(0),
        EXTIOI_VIRT_FEATURES => Some(
            EXTIOI_HAS_VIRT_EXTENSION
                | EXTIOI_HAS_ENABLE_OPTION
                | EXTIOI_HAS_INT_ENCODE
                | EXTIOI_HAS_CPU_ENCODE,
        ),
        EXTIOI_VIRT_CONFIG => Some(EIOINTC_VIRT_CONFIG_WORDS[index].load(Ordering::Acquire)),
        EIOINTC_NODEMAP_BASE..EIOINTC_NODEMAP_END => Some(read_atomic_u32_slots(
            &EIOINTC_NODEMAP_WORDS_STORAGE,
            index,
            EIOINTC_NODEMAP_WORDS,
            addr,
            EIOINTC_NODEMAP_BASE,
            len,
        )),
        EIOINTC_IPMAP_BASE..EIOINTC_IPMAP_END => Some(read_atomic_u32_slots(
            &EIOINTC_IPMAP_WORDS_STORAGE,
            index,
            EIOINTC_IPMAP_WORDS,
            addr,
            EIOINTC_IPMAP_BASE,
            len,
        )),
        EIOINTC_ENABLE_BASE..EIOINTC_ENABLE_END => Some(read_atomic_u32_slots(
            &EIOINTC_ENABLE_WORDS,
            index,
            EIOINTC_ENABLE_WORDS_PER_VCPU,
            addr,
            EIOINTC_ENABLE_BASE,
            len,
        )),
        EIOINTC_BOUNCE_BASE..EIOINTC_BOUNCE_END => Some(read_atomic_u32_slots(
            &EIOINTC_BOUNCE_WORDS_STORAGE,
            index,
            EIOINTC_BOUNCE_WORDS,
            addr,
            EIOINTC_BOUNCE_BASE,
            len,
        )),
        EIOINTC_ISR_COMPAT_BASE..EIOINTC_ISR_COMPAT_END => {
            let value = read_eiointc_isr_slots(index, addr, EIOINTC_ISR_COMPAT_BASE, len);
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
            let value = read_eiointc_isr_slots(index, addr, EIOINTC_ISR_BASE, len);
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
            &EIOINTC_COREMAP_WORDS_STORAGE,
            index,
            EIOINTC_COREMAP_WORDS,
            addr,
            EIOINTC_COREMAP_BASE,
            len,
        )),
        _ => None,
    }
}

fn write_guest_iocsr(
    ctx: &mut LoongArchContextFrame,
    vm_id: VMId,
    vcpu_id: VCpuId,
    addr: usize,
    len: usize,
    value: usize,
) -> Option<AxVCpuExitReason> {
    let index = iocsr_vcpu_index(vm_id, vcpu_id)?;
    match addr {
        LOONGARCH_IOCSR_IPI_STATUS => Some(AxVCpuExitReason::Nothing),
        LOONGARCH_IOCSR_IPI_EN => {
            IOCSR_IPI_ENABLE_WORDS[index].store(value, Ordering::Release);
            Some(AxVCpuExitReason::Nothing)
        }
        LOONGARCH_IOCSR_IPI_SET => {
            IOCSR_IPI_STATUS_WORDS[index].fetch_or(value, Ordering::AcqRel);
            ctx.gcsr_estat |= IPI_BIT;
            Some(AxVCpuExitReason::Nothing)
        }
        LOONGARCH_IOCSR_IPI_CLEAR => {
            let new_status =
                IOCSR_IPI_STATUS_WORDS[index].fetch_and(!value, Ordering::AcqRel) & !value;
            if new_status == 0 {
                ctx.gcsr_estat &= !IPI_BIT;
            }
            Some(AxVCpuExitReason::Nothing)
        }
        LOONGARCH_IOCSR_IPI_SEND => {
            let target_cpu = (value >> IOCSR_SEND_CPU_SHIFT) & IOCSR_SEND_CPU_MASK;
            let action = value & IOCSR_IPI_ACTION_MASK;
            if let Some(target_index) = iocsr_vcpu_index(vm_id, target_cpu) {
                IOCSR_IPI_STATUS_WORDS[target_index].fetch_or(1usize << action, Ordering::AcqRel);
                if target_cpu == vcpu_id {
                    ctx.gcsr_estat |= IPI_BIT;
                } else {
                    host::inject_interrupt(vm_id, target_cpu, INT_IPI);
                }
            } else {
                log::debug!(
                    "LoongArch guest IOCSR IPI_SEND ignored for unsupported target_cpu={} \
                     action={}",
                    target_cpu,
                    action
                );
            }
            Some(AxVCpuExitReason::Nothing)
        }
        LOONGARCH_IOCSR_MAIL_BUF0..=LOONGARCH_IOCSR_MAIL_BUF3 => {
            if let Some(mail_index) = iocsr_mail_buf_index(index, addr) {
                IOCSR_MAIL_BUF_WORDS[mail_index].store(value, Ordering::Release);
            }
            Some(AxVCpuExitReason::Nothing)
        }
        LOONGARCH_IOCSR_MBUF_SEND => {
            let target_cpu = (value >> IOCSR_SEND_CPU_SHIFT) & IOCSR_SEND_CPU_MASK;
            let mail_word = (value >> IOCSR_MBUF_SEND_BOX_SHIFT) & 0x7;
            let mail_buf = mail_word / 2;
            let is_high_word = mail_word % 2 == 1;
            if mail_buf < IOCSR_MAIL_BUF_COUNT {
                if let Some(target_index) = iocsr_vcpu_index(vm_id, target_cpu) {
                    let mail_index = target_index * IOCSR_MAIL_BUF_COUNT + mail_buf;
                    let old = IOCSR_MAIL_BUF_WORDS[mail_index].load(Ordering::Acquire);
                    let new = if is_high_word {
                        (old & !IOCSR_MBUF_SEND_H32_MASK) | (value & IOCSR_MBUF_SEND_H32_MASK)
                    } else {
                        let low = value >> IOCSR_MBUF_SEND_BUF_SHIFT;
                        (old & IOCSR_MBUF_SEND_H32_MASK) | low
                    };
                    IOCSR_MAIL_BUF_WORDS[mail_index].store(new, Ordering::Release);
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
            Some(AxVCpuExitReason::Nothing)
        }
        LOONGARCH_IOCSR_ANY_SEND => {
            let target_cpu = (value >> IOCSR_SEND_CPU_SHIFT) & IOCSR_SEND_CPU_MASK;
            let target = value & 0xffff;
            if target_cpu == vcpu_id {
                write_guest_iocsr_send_data(
                    ctx,
                    vm_id,
                    vcpu_id,
                    target,
                    value,
                    LOONGARCH_IOCSR_ANY_SEND_BUF_SHIFT,
                );
            }
            Some(AxVCpuExitReason::Nothing)
        }
        EXTIOI_VIRT_CONFIG => {
            EIOINTC_VIRT_CONFIG_WORDS[index].store(value, Ordering::Release);
            update_guest_eiointc_irq(ctx, index);
            Some(AxVCpuExitReason::Nothing)
        }
        EIOINTC_NODEMAP_BASE..EIOINTC_NODEMAP_END => {
            write_atomic_u32_slots(
                &EIOINTC_NODEMAP_WORDS_STORAGE,
                index,
                EIOINTC_NODEMAP_WORDS,
                addr,
                EIOINTC_NODEMAP_BASE,
                len,
                value,
            );
            Some(AxVCpuExitReason::Nothing)
        }
        EIOINTC_IPMAP_BASE..EIOINTC_IPMAP_END => {
            write_atomic_u32_slots(
                &EIOINTC_IPMAP_WORDS_STORAGE,
                index,
                EIOINTC_IPMAP_WORDS,
                addr,
                EIOINTC_IPMAP_BASE,
                len,
                value,
            );
            update_guest_eiointc_irq(ctx, index);
            Some(AxVCpuExitReason::Nothing)
        }
        EIOINTC_ENABLE_BASE..EIOINTC_ENABLE_END => {
            write_atomic_u32_slots(
                &EIOINTC_ENABLE_WORDS,
                index,
                EIOINTC_ENABLE_WORDS_PER_VCPU,
                addr,
                EIOINTC_ENABLE_BASE,
                len,
                value,
            );
            update_guest_eiointc_irq(ctx, index);
            Some(AxVCpuExitReason::Nothing)
        }
        EIOINTC_BOUNCE_BASE..EIOINTC_BOUNCE_END => {
            write_atomic_u32_slots(
                &EIOINTC_BOUNCE_WORDS_STORAGE,
                index,
                EIOINTC_BOUNCE_WORDS,
                addr,
                EIOINTC_BOUNCE_BASE,
                len,
                value,
            );
            Some(AxVCpuExitReason::Nothing)
        }
        EIOINTC_ISR_COMPAT_BASE..EIOINTC_ISR_COMPAT_END => {
            clear_eiointc_isr_slots(index, addr, EIOINTC_ISR_COMPAT_BASE, len, value);
            update_guest_eiointc_irq(ctx, index);
            Some(AxVCpuExitReason::Nothing)
        }
        EIOINTC_ISR_BASE..EIOINTC_ISR_END if is_eiointc_isr_addr(addr) => {
            clear_eiointc_isr_slots(index, addr, EIOINTC_ISR_BASE, len, value);
            update_guest_eiointc_irq(ctx, index);
            log_eiointc_trace(
                "clear",
                vm_id,
                vcpu_id,
                (addr - EIOINTC_ISR_BASE) / 8,
                addr - EIOINTC_ISR_BASE,
                read_eiointc_isr_slots(index, addr, EIOINTC_ISR_BASE, len),
            );
            Some(AxVCpuExitReason::Nothing)
        }
        EIOINTC_COREMAP_BASE..EIOINTC_COREMAP_END => {
            write_atomic_u32_slots(
                &EIOINTC_COREMAP_WORDS_STORAGE,
                index,
                EIOINTC_COREMAP_WORDS,
                addr,
                EIOINTC_COREMAP_BASE,
                len,
                value,
            );
            update_guest_eiointc_irq(ctx, index);
            Some(AxVCpuExitReason::Nothing)
        }
        EIOINTC_GUEST_OWNED_BASE..EIOINTC_GUEST_OWNED_END => Some(AxVCpuExitReason::Nothing),
        _ => None,
    }
}

fn write_guest_iocsr_send_data(
    ctx: &mut LoongArchContextFrame,
    vm_id: VMId,
    vcpu_id: VCpuId,
    target: usize,
    value: usize,
    data_shift: usize,
) {
    let preserve_mask = (value >> IOCSR_SEND_BYTE_MASK_SHIFT) & 0xf;
    let mut data = (value >> data_shift) as u32 as usize;

    if preserve_mask != 0 {
        let old = read_guest_iocsr(vm_id, vcpu_id, target, 4).unwrap_or(0) as u32;
        let mut byte_mask = 0u32;
        for byte in 0..4 {
            if preserve_mask & (1 << byte) != 0 {
                byte_mask |= 0xff << (byte * 8);
            }
        }
        data = ((old & byte_mask) as usize) | (data & !(byte_mask as usize));
    }

    let _ = write_guest_iocsr(ctx, vm_id, vcpu_id, target, 4, data);
}

fn should_inject_guest_virtual_fault(
    ctx: &LoongArchContextFrame,
    badv: usize,
    from_tlb_refill: bool,
) -> bool {
    let is_direct = is_guest_direct_mapped_va(badv);
    let known_physical = is_known_guest_physical_addr(badv);

    if from_tlb_refill {
        if ctx.gcsr_tlbrentry == 0 || is_direct {
            false
        } else {
            !known_physical
        }
    } else if !guest_paging_enabled(ctx) || is_direct || ctx.gcsr_eentry == 0 {
        false
    } else {
        !known_physical
    }
}

fn guest_exception_vector_size(ctx: &LoongArchContextFrame) -> usize {
    let vs = (ctx.gcsr_ectl & CSR_ECFG_VS_MASK) >> CSR_ECFG_VS_SHIFT;
    if vs == 0 { 0 } else { (1 << vs) * 4 }
}

fn guest_pgd(ctx: &LoongArchContextFrame) -> usize {
    let badv = if ctx.gcsr_tlbrera & CSR_TLBRERA_ISTLBR != 0 {
        ctx.gcsr_tlbrbadv
    } else {
        ctx.gcsr_badv
    };

    if badv >> (usize::BITS - 1) != 0 {
        ctx.gcsr_pgdh
    } else {
        ctx.gcsr_pgdl
    }
}

fn inject_guest_regular_exception(
    ctx: &mut LoongArchContextFrame,
    ecode: usize,
    esubcode: usize,
    badv: usize,
) {
    let pc = get_guest_pc(ctx);
    ctx.gcsr_badv = badv;
    ctx.gcsr_badi = get_badi(ctx);
    ctx.gcsr_tlbehi = badv & !0x1fff;
    ctx.gcsr_estat = (ctx.gcsr_estat & !CSR_ESTAT_EXC_MASK)
        | ((ecode & 0x3f) << 16)
        | ((esubcode & 0x1ff) << 22);
    ctx.gcsr_prmd = (ctx.gcsr_prmd & !0b111) | (ctx.gcsr_crmd & (CSR_CRMD_PLV_MASK | CSR_CRMD_IE));
    ctx.gcsr_era = pc;
    ctx.gcsr_crmd &= !(CSR_CRMD_PLV_MASK | CSR_CRMD_IE);
    ctx.sepc = ctx.gcsr_eentry + ecode * guest_exception_vector_size(ctx);
}

fn inject_guest_interrupt_at(ctx: &mut LoongArchContextFrame, vector: usize, pc: usize) {
    ctx.gcsr_prmd = (ctx.gcsr_prmd & !0b111) | (ctx.gcsr_crmd & (CSR_CRMD_PLV_MASK | CSR_CRMD_IE));
    ctx.gcsr_era = pc;
    ctx.gcsr_crmd &= !(CSR_CRMD_PLV_MASK | CSR_CRMD_IE);
    ctx.sepc = ctx.gcsr_eentry + (64 + vector) * guest_exception_vector_size(ctx);
}

pub(crate) fn inject_enabled_pending_interrupt(
    ctx: &mut LoongArchContextFrame,
    vm_id: VMId,
    vcpu_id: VCpuId,
) -> bool {
    if let Some(index) = iocsr_vcpu_index(vm_id, vcpu_id) {
        update_guest_eiointc_irq(ctx, index);
    }
    let pending_enabled = ctx.gcsr_estat & ctx.gcsr_ectl & LOCAL_INTERRUPT_MASK;
    if ctx.gcsr_eentry != 0
        && ctx.gcsr_crmd & CSR_CRMD_IE != 0
        && let Some(vector) = decode_interrupt_vector(pending_enabled)
    {
        inject_guest_interrupt_at(ctx, vector, get_guest_pc(ctx));
        true
    } else {
        false
    }
}

fn inject_guest_tlb_refill(ctx: &mut LoongArchContextFrame, badv: usize) {
    let pc = get_guest_pc(ctx);
    let page_shift = match ctx.gcsr_stlbps & CSR_TLBREHI_PS_MASK {
        0 => DEFAULT_TLB_PAGE_SHIFT,
        shift => shift,
    };
    let pair_mask = (1usize << (page_shift + 1)) - 1;
    let vppn = (badv & !pair_mask) & CSR_TLBREHI_VPPN_MASK;

    ctx.gcsr_tlbrbadv = badv;
    ctx.gcsr_tlbrehi =
        (ctx.gcsr_tlbrehi & !(CSR_TLBREHI_VPPN_MASK | CSR_TLBREHI_PS_MASK)) | vppn | page_shift;
    ctx.gcsr_tlbrera = (pc & !0x3) | CSR_TLBRERA_ISTLBR;
    ctx.gcsr_tlbrprmd =
        (ctx.gcsr_tlbrprmd & !0b111) | (ctx.gcsr_crmd & (CSR_CRMD_PLV_MASK | CSR_CRMD_IE));
    ctx.gcsr_pgd = guest_pgd(ctx);
    ctx.gcsr_crmd =
        (ctx.gcsr_crmd | CSR_CRMD_DA) & !(CSR_CRMD_PG | CSR_CRMD_PLV_MASK | CSR_CRMD_IE);
    ctx.sepc = ctx.gcsr_tlbrentry;
}

fn get_badv(ctx: &LoongArchContextFrame) -> usize {
    if ctx.host_tlbrera & 0x1 != 0 {
        ctx.host_tlbrbadv
    } else {
        ctx.host_badv
    }
}

fn get_badi(ctx: &LoongArchContextFrame) -> usize {
    ctx.host_badi
}

pub(crate) fn current_badi() -> usize {
    unsafe { crate::registers::csr_read::<CSR_BADI_U16>() }
}

fn get_guest_interrupt_status(ctx: &LoongArchContextFrame) -> usize {
    ctx.host_estat & LOCAL_INTERRUPT_MASK
}

fn ack_host_timer_interrupt() {
    unsafe {
        let value = CSR_TICLR_TI;
        core::arch::asm!("csrwr {}, 0x44", inout(reg) value => _);
    }
}

fn decode_interrupt_vector(is: usize) -> Option<usize> {
    if is & IPI_BIT != 0 {
        return Some(INT_IPI);
    }
    if is & TIMER_BIT != 0 {
        return Some(INT_TIMER);
    }

    let hwi = is & HWI_MASK;
    if hwi != 0 {
        return Some(hwi.trailing_zeros() as usize);
    }

    None
}

fn extract_field(value: usize, offset: usize, width: usize) -> usize {
    (value >> offset) & ((1usize << width) - 1)
}

fn advance_guest_pc(ctx: &mut LoongArchContextFrame) {
    ctx.advance_guest_pc();
}

fn emulate_cpucfg(ctx: &mut LoongArchContextFrame, ins: usize) -> AxVCpuExitReason {
    let rd = extract_field(ins, 0, 5);
    let rj = extract_field(ins, 5, 5);
    let cpucfg_idx = ctx.x[rj];
    let mut value = if cpucfg_idx > 20 {
        0
    } else {
        let result: usize;
        unsafe {
            core::arch::asm!("cpucfg {}, {}", out(reg) result, in(reg) cpucfg_idx);
        }
        result
    };
    if cpucfg_idx == 2 {
        value &= !CPUCFG2_CRYPTO;
    }
    ctx.set_gpr(rd, value);
    advance_guest_pc(ctx);
    AxVCpuExitReason::Nothing
}

fn emulate_csrx(
    ctx: &mut LoongArchContextFrame,
    ins: usize,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) -> AxVCpuExitReason {
    let rd = extract_field(ins, 0, 5);
    let rj = extract_field(ins, 5, 5);
    let csr = extract_field(ins, 10, 14);

    emulate_guest_csr(ctx, rd, rj, csr, vm_id, vcpu_id, guest_timer_token);
    advance_guest_pc(ctx);
    AxVCpuExitReason::Nothing
}

/// Timer CSR numbers (matching GCSR encoding in LoongArch LVZ).
const CSR_TCFG: usize = 0x41;
const CSR_TVAL: usize = 0x42;
const CSR_TICLR: usize = 0x44;

fn read_guest_csr(ctx: &LoongArchContextFrame, csr: usize) -> usize {
    match csr {
        CSR_CRMD => ctx.gcsr_crmd,
        CSR_PRMD => ctx.gcsr_prmd,
        CSR_EUEN => ctx.gcsr_euen,
        CSR_MISC => ctx.gcsr_misc,
        CSR_ECFG => ctx.gcsr_ectl,
        CSR_ESTAT => ctx.gcsr_estat,
        CSR_ERA => ctx.gcsr_era,
        CSR_BADV => ctx.gcsr_badv,
        CSR_BADI => ctx.gcsr_badi,
        CSR_EENTRY => ctx.gcsr_eentry,
        CSR_TLBIDX => ctx.gcsr_tlbidx,
        CSR_TLBEHI => ctx.gcsr_tlbehi,
        CSR_TLBELO0 => ctx.gcsr_tlbelo0,
        CSR_TLBELO1 => ctx.gcsr_tlbelo1,
        CSR_ASID => ctx.gcsr_asid,
        CSR_PGDL => ctx.gcsr_pgdl,
        CSR_PGDH => ctx.gcsr_pgdh,
        CSR_PGD => guest_pgd(ctx),
        CSR_PWCL => ctx.gcsr_pwcl,
        CSR_PWCH => ctx.gcsr_pwch,
        CSR_STLBPS => ctx.gcsr_stlbps,
        CSR_RAVCFG => ctx.gcsr_ravcfg,
        CSR_CPUID => ctx.gcsr_cpuid,
        CSR_PRCFG1 => ctx.gcsr_prcfg1,
        CSR_PRCFG2 => ctx.gcsr_prcfg2,
        CSR_PRCFG3 => ctx.gcsr_prcfg3,
        0x30 => ctx.gcsr_save0,
        0x31 => ctx.gcsr_save1,
        0x32 => ctx.gcsr_save2,
        0x33 => ctx.gcsr_save3,
        0x34 => ctx.gcsr_save4,
        0x35 => ctx.gcsr_save5,
        0x36 => ctx.gcsr_save6,
        0x37 => ctx.gcsr_save7,
        0x38 => ctx.gcsr_save8,
        0x39 => ctx.gcsr_save9,
        0x3a => ctx.gcsr_save10,
        0x3b => ctx.gcsr_save11,
        0x3c => ctx.gcsr_save12,
        0x3d => ctx.gcsr_save13,
        0x3e => ctx.gcsr_save14,
        0x3f => ctx.gcsr_save15,
        CSR_TID => ctx.gcsr_tid,
        CSR_TCFG => ctx.gcsr_tcfg,
        CSR_TVAL => ctx.gcsr_tval,
        CSR_TICLR => ctx.gcsr_ticlr,
        CSR_LLBCTL => ctx.gcsr_llbctl,
        CSR_TLBRENTRY => ctx.gcsr_tlbrentry,
        CSR_TLBRBADV => ctx.gcsr_tlbrbadv,
        CSR_TLBRERA => ctx.gcsr_tlbrera,
        CSR_TLBRSAVE => ctx.gcsr_tlbrsave,
        CSR_TLBRELO0 => ctx.gcsr_tlbrelo0,
        CSR_TLBRELO1 => ctx.gcsr_tlbrelo1,
        CSR_TLBREHI => ctx.gcsr_tlbrehi,
        CSR_TLBRPRMD => ctx.gcsr_tlbrprmd,
        CSR_DMW0 => ctx.gcsr_dmw0,
        CSR_DMW1 => ctx.gcsr_dmw1,
        CSR_DMW2 => ctx.gcsr_dmw2,
        CSR_DMW3 => ctx.gcsr_dmw3,
        _ => 0,
    }
}

fn write_guest_csr(
    ctx: &mut LoongArchContextFrame,
    csr: usize,
    value: usize,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) {
    match csr {
        CSR_CRMD => ctx.gcsr_crmd = value,
        CSR_PRMD => ctx.gcsr_prmd = value,
        CSR_EUEN => ctx.gcsr_euen = value,
        CSR_MISC => ctx.gcsr_misc = value,
        CSR_ECFG => ctx.gcsr_ectl = value,
        CSR_ESTAT => {
            ctx.gcsr_estat = (ctx.gcsr_estat & !0x3) | (value & 0x3);
        }
        CSR_ERA => ctx.gcsr_era = value,
        CSR_BADV => ctx.gcsr_badv = value,
        CSR_BADI => ctx.gcsr_badi = value,
        CSR_EENTRY => ctx.gcsr_eentry = value,
        CSR_TLBIDX => ctx.gcsr_tlbidx = value,
        CSR_TLBEHI => ctx.gcsr_tlbehi = value,
        CSR_TLBELO0 => ctx.gcsr_tlbelo0 = value,
        CSR_TLBELO1 => ctx.gcsr_tlbelo1 = value,
        CSR_ASID => ctx.gcsr_asid = value,
        CSR_PGDL => ctx.gcsr_pgdl = value,
        CSR_PGDH => ctx.gcsr_pgdh = value,
        CSR_PGD => ctx.gcsr_pgd = value,
        CSR_PWCL => ctx.gcsr_pwcl = value,
        CSR_PWCH => ctx.gcsr_pwch = value,
        CSR_STLBPS => ctx.gcsr_stlbps = value,
        CSR_RAVCFG => ctx.gcsr_ravcfg = value,
        CSR_CPUID => ctx.gcsr_cpuid = value,
        CSR_PRCFG1 => ctx.gcsr_prcfg1 = value,
        CSR_PRCFG2 => ctx.gcsr_prcfg2 = value,
        CSR_PRCFG3 => ctx.gcsr_prcfg3 = value,
        0x30 => ctx.gcsr_save0 = value,
        0x31 => ctx.gcsr_save1 = value,
        0x32 => ctx.gcsr_save2 = value,
        0x33 => ctx.gcsr_save3 = value,
        0x34 => ctx.gcsr_save4 = value,
        0x35 => ctx.gcsr_save5 = value,
        0x36 => ctx.gcsr_save6 = value,
        0x37 => ctx.gcsr_save7 = value,
        0x38 => ctx.gcsr_save8 = value,
        0x39 => ctx.gcsr_save9 = value,
        0x3a => ctx.gcsr_save10 = value,
        0x3b => ctx.gcsr_save11 = value,
        0x3c => ctx.gcsr_save12 = value,
        0x3d => ctx.gcsr_save13 = value,
        0x3e => ctx.gcsr_save14 = value,
        0x3f => ctx.gcsr_save15 = value,
        CSR_TID => ctx.gcsr_tid = value,
        CSR_TCFG | CSR_TVAL | CSR_TICLR => {
            write_guest_timer_csr(ctx, csr, value, vm_id, vcpu_id, guest_timer_token)
        }
        CSR_LLBCTL => ctx.gcsr_llbctl = value,
        CSR_TLBRENTRY => ctx.gcsr_tlbrentry = value,
        CSR_TLBRBADV => ctx.gcsr_tlbrbadv = value,
        CSR_TLBRERA => ctx.gcsr_tlbrera = value,
        CSR_TLBRSAVE => ctx.gcsr_tlbrsave = value,
        CSR_TLBRELO0 => ctx.gcsr_tlbrelo0 = value,
        CSR_TLBRELO1 => ctx.gcsr_tlbrelo1 = value,
        CSR_TLBREHI => ctx.gcsr_tlbrehi = value,
        CSR_TLBRPRMD => ctx.gcsr_tlbrprmd = value,
        CSR_DMW0 => ctx.gcsr_dmw0 = value,
        CSR_DMW1 => ctx.gcsr_dmw1 = value,
        CSR_DMW2 => ctx.gcsr_dmw2 = value,
        CSR_DMW3 => ctx.gcsr_dmw3 = value,
        _ => log::debug!(
            "LoongArch GSPR CSR write ignored: csr={:#x}, value={:#x}",
            csr,
            value
        ),
    }
}

fn guest_timer_periodic(ctx: &LoongArchContextFrame) -> bool {
    ctx.gcsr_tcfg & CSR_TCFG_PERIODIC != 0
}

fn guest_timer_init_ticks(ctx: &LoongArchContextFrame) -> u64 {
    (ctx.gcsr_tcfg & CSR_TCFG_INITVAL_MASK) as u64
}

fn cancel_guest_timer(guest_timer_token: &mut Option<usize>) {
    if let Some(token) = guest_timer_token.take() {
        host::cancel_timer(token);
    }
}

fn register_guest_timer(
    ctx: &mut LoongArchContextFrame,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) {
    cancel_guest_timer(guest_timer_token);

    if ctx.gcsr_tcfg & CSR_TCFG_EN == 0 {
        return;
    }

    let init_ticks = guest_timer_init_ticks(ctx);
    if init_ticks == 0 {
        ctx.gcsr_tval = 0;
        ctx.gcsr_estat |= TIMER_BIT;
        if GUEST_TIMER_LOGS.fetch_add(1, Ordering::Relaxed) < 64 {
            log::trace!(
                "LoongArch guest timer immediate: tcfg={:#x}, estat={:#x}",
                ctx.gcsr_tcfg,
                ctx.gcsr_estat
            );
        }
        return;
    }

    ctx.gcsr_tval = init_ticks as usize;
    let delay_ns = host::ticks_to_nanos(init_ticks);
    let deadline_ns = host::current_time_nanos().saturating_add(delay_ns);
    if GUEST_TIMER_LOGS.fetch_add(1, Ordering::Relaxed) < 64 {
        log::trace!(
            "LoongArch guest timer arm: tcfg={:#x}, init_ticks={}, delay_ns={}, deadline_ns={}",
            ctx.gcsr_tcfg,
            init_ticks,
            delay_ns,
            deadline_ns
        );
    }
    let token = host::register_timer(
        Duration::from_nanos(deadline_ns),
        Box::new(move |_| host::inject_interrupt(vm_id, vcpu_id, INT_TIMER)),
    );
    *guest_timer_token = Some(token);
}

fn write_guest_timer_csr(
    ctx: &mut LoongArchContextFrame,
    csr: usize,
    value: usize,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) {
    match csr {
        CSR_TCFG => {
            ctx.gcsr_tcfg = value;
            register_guest_timer(ctx, vm_id, vcpu_id, guest_timer_token);
        }
        CSR_TVAL => {
            ctx.gcsr_tval = value;
        }
        CSR_TICLR => {
            ctx.gcsr_ticlr = value;
            if value & CSR_TICLR_TI != 0 {
                ctx.gcsr_estat &= !TIMER_BIT;
                if GUEST_TIMER_LOGS.fetch_add(1, Ordering::Relaxed) < 64 {
                    log::warn!(
                        "LoongArch guest timer clear: tcfg={:#x}, ticlr={:#x}, periodic={}, \
                         estat={:#x}",
                        ctx.gcsr_tcfg,
                        value,
                        guest_timer_periodic(ctx),
                        ctx.gcsr_estat
                    );
                }
                if guest_timer_periodic(ctx) {
                    register_guest_timer(ctx, vm_id, vcpu_id, guest_timer_token);
                } else {
                    ctx.gcsr_tcfg &= !CSR_TCFG_EN;
                    cancel_guest_timer(guest_timer_token);
                }
            }
        }
        _ => {}
    }
}

fn emulate_guest_csr(
    ctx: &mut LoongArchContextFrame,
    rd: usize,
    rj: usize,
    csr: usize,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) {
    let old_value = read_guest_csr(ctx, csr);
    let mut return_value = old_value;

    if rj != 0 {
        let new_value = if rj == 1 {
            ctx.x[rd]
        } else {
            let mask = ctx.x[rj];
            return_value &= mask;
            (old_value & !mask) | (ctx.x[rd] & mask)
        };
        write_guest_csr(ctx, csr, new_value, vm_id, vcpu_id, guest_timer_token);
    }

    ctx.set_gpr(rd, return_value);
}

fn emulate_cacop(ctx: &mut LoongArchContextFrame, _ins: usize) -> AxVCpuExitReason {
    log::trace!(
        "LoongArch GSPR cacop emulation skipped at guest_pc={:#x}",
        get_guest_pc(ctx)
    );
    advance_guest_pc(ctx);
    AxVCpuExitReason::Nothing
}

fn emulate_idle(ctx: &mut LoongArchContextFrame, ins: usize) -> AxVCpuExitReason {
    let level = extract_field(ins, 0, 15);
    let pending_enabled = ctx.gcsr_estat & ctx.gcsr_ectl & LOCAL_INTERRUPT_MASK;
    let idle_log_index = IDLE_EXIT_LOGS.fetch_add(1, Ordering::Relaxed);
    if idle_log_index < 64 || idle_log_index.is_power_of_two() {
        log::trace!(
            "LoongArch guest idle: pc={:#x}, level={:#x}, pending_enabled={:#x}, eentry={:#x}, \
             crmd={:#x}, estat={:#x}, ecfg={:#x}, tcfg={:#x}, tval={:#x}, ticlr={:#x}",
            get_guest_pc(ctx),
            level,
            pending_enabled,
            ctx.gcsr_eentry,
            ctx.gcsr_crmd,
            ctx.gcsr_estat,
            ctx.gcsr_ectl,
            ctx.gcsr_tcfg,
            ctx.gcsr_tval,
            ctx.gcsr_ticlr,
        );
    }
    if ctx.gcsr_eentry != 0
        && ctx.gcsr_crmd & CSR_CRMD_IE != 0
        && let Some(vector) = decode_interrupt_vector(pending_enabled)
    {
        if idle_log_index < 64 || idle_log_index.is_power_of_two() {
            log::trace!(
                "LoongArch guest idle has pending interrupt: pc={:#x}, vector={}, \
                 pending_enabled={:#x}",
                get_guest_pc(ctx),
                vector,
                pending_enabled,
            );
        }
        inject_guest_interrupt_at(ctx, vector, get_guest_pc(ctx).wrapping_add(4));
        return AxVCpuExitReason::Nothing;
    }
    advance_guest_pc(ctx);
    AxVCpuExitReason::Idle
}

fn emulate_iocsr(
    ctx: &mut LoongArchContextFrame,
    ins: usize,
    vm_id: VMId,
    vcpu_id: VCpuId,
) -> AxVCpuExitReason {
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
            let value = read_guest_iocsr(vm_id, vcpu_id, rj_value, len).unwrap_or_else(|| {
                let value: usize;
                unsafe {
                    core::arch::asm!("iocsrrd.b {}, {}", out(reg) value, in(reg) rj_value);
                }
                value
            });
            log_iocsr(ctx, "read.b", value);
            target_log(ctx, "read.b", value);
            ctx.set_gpr(rd, (value as i8) as isize as usize);
        }
        1 => {
            let value = read_guest_iocsr(vm_id, vcpu_id, rj_value, len).unwrap_or_else(|| {
                let value: usize;
                unsafe {
                    core::arch::asm!("iocsrrd.h {}, {}", out(reg) value, in(reg) rj_value);
                }
                value
            });
            log_iocsr(ctx, "read.h", value);
            target_log(ctx, "read.h", value);
            ctx.set_gpr(rd, (value as i16) as isize as usize);
        }
        2 => {
            let value = read_guest_iocsr(vm_id, vcpu_id, rj_value, len).unwrap_or_else(|| {
                let value: usize;
                unsafe {
                    core::arch::asm!("iocsrrd.w {}, {}", out(reg) value, in(reg) rj_value);
                }
                value
            });
            log_iocsr(ctx, "read.w", value);
            target_log(ctx, "read.w", value);
            ctx.set_gpr(rd, (value as i32) as isize as usize);
        }
        3 => {
            let value = read_guest_iocsr(vm_id, vcpu_id, rj_value, len).unwrap_or_else(|| {
                let value: usize;
                unsafe {
                    core::arch::asm!("iocsrrd.d {}, {}", out(reg) value, in(reg) rj_value);
                }
                value
            });
            log_iocsr(ctx, "read.d", value);
            target_log(ctx, "read.d", value);
            ctx.set_gpr(rd, value);
        }
        4 => {
            if let Some(reason) =
                write_guest_iocsr(ctx, vm_id, vcpu_id, rj_value, len, ctx.x[rd] as u8 as usize)
            {
                advance_guest_pc(ctx);
                return reason;
            } else {
                log_iocsr(ctx, "write.b", ctx.x[rd] as u8 as usize);
                unsafe {
                    core::arch::asm!("iocsrwr.b {}, {}", in(reg) ctx.x[rd], in(reg) rj_value);
                }
            }
        }
        5 => {
            if let Some(reason) = write_guest_iocsr(
                ctx,
                vm_id,
                vcpu_id,
                rj_value,
                len,
                ctx.x[rd] as u16 as usize,
            ) {
                advance_guest_pc(ctx);
                return reason;
            } else {
                log_iocsr(ctx, "write.h", ctx.x[rd] as u16 as usize);
                unsafe {
                    core::arch::asm!("iocsrwr.h {}, {}", in(reg) ctx.x[rd], in(reg) rj_value);
                }
            }
        }
        6 => {
            if let Some(reason) = write_guest_iocsr(
                ctx,
                vm_id,
                vcpu_id,
                rj_value,
                len,
                ctx.x[rd] as u32 as usize,
            ) {
                advance_guest_pc(ctx);
                return reason;
            } else {
                log_iocsr(ctx, "write.w", ctx.x[rd] as u32 as usize);
                unsafe {
                    core::arch::asm!("iocsrwr.w {}, {}", in(reg) ctx.x[rd], in(reg) rj_value);
                }
            }
        }
        7 => {
            if let Some(reason) = write_guest_iocsr(ctx, vm_id, vcpu_id, rj_value, len, ctx.x[rd]) {
                advance_guest_pc(ctx);
                return reason;
            } else {
                let is_eiointc_complete = is_eiointc_isr_addr(rj_value);
                log_iocsr(ctx, "write.d", ctx.x[rd]);
                unsafe {
                    core::arch::asm!("iocsrwr.d {}, {}", in(reg) ctx.x[rd], in(reg) rj_value);
                }
                if is_eiointc_complete && !host_eiointc_has_pending() {
                    ctx.gcsr_estat &= !HWI_MASK;
                }
            }
        }
        _ => panic!("invalid LoongArch IOCSR opcode type: {ty}"),
    }

    advance_guest_pc(ctx);
    AxVCpuExitReason::Nothing
}

fn log_eiointc_trace(
    op: &str,
    vm_id: VMId,
    vcpu_id: VCpuId,
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

fn emulate_gspr(
    ctx: &mut LoongArchContextFrame,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) -> AxVCpuExitReason {
    let ins = get_badi(ctx) as u32 as usize;
    const OPCODE_CPUCFG: usize = 0b0000000000000000011011;
    const OPCODE_CPUCFG_LEN: usize = 22;
    const OPCODE_CACOP: usize = 0b0000011000;
    const OPCODE_CACOP_LEN: usize = 10;
    const OPCODE_IDLE: usize = 0b0_0000_1100_1001_0001;
    const OPCODE_IDLE_LEN: usize = 17;
    const OPCODE_CSRX: usize = 0b00000100;
    const OPCODE_CSRX_LEN: usize = 8;
    const OPCODE_IOCSR: usize = 0b0000011001;
    const OPCODE_IOCSR_LEN: usize = 10;

    let matches = |opcode: usize, len: usize| -> bool {
        let shift = 32 - len;
        ((ins >> shift) & ((1usize << len) - 1)) == opcode
    };
    let pc = get_guest_pc(ctx);
    if (0x9000_0000_0159_0000..0x9000_0000_015a_0000).contains(&pc)
        && TARGET_GSPR_LOGS.fetch_add(1, Ordering::Relaxed) < 64
    {
        log::trace!(
            "LoongArch target GSPR: pc={:#x}, ins={:#x}, rd={}, rj={}, csr={:#x}, iocsr_ty={}, \
             a0={:#x}, a1={:#x}, a2={:#x}, estat={:#x}, ecfg={:#x}, tcfg={:#x}, tval={:#x}",
            pc,
            ins,
            extract_field(ins, 0, 5),
            extract_field(ins, 5, 5),
            extract_field(ins, 10, 14),
            extract_field(ins, 10, 3),
            ctx.get_a0(),
            ctx.get_a1(),
            ctx.get_a2(),
            ctx.gcsr_estat,
            ctx.gcsr_ectl,
            ctx.gcsr_tcfg,
            ctx.gcsr_tval,
        );
    }

    if matches(OPCODE_CPUCFG, OPCODE_CPUCFG_LEN) {
        return emulate_cpucfg(ctx, ins);
    }
    if matches(OPCODE_CACOP, OPCODE_CACOP_LEN) {
        return emulate_cacop(ctx, ins);
    }
    if matches(OPCODE_IDLE, OPCODE_IDLE_LEN) {
        return emulate_idle(ctx, ins);
    }
    if matches(OPCODE_CSRX, OPCODE_CSRX_LEN) {
        return emulate_csrx(ctx, ins, vm_id, vcpu_id, guest_timer_token);
    }
    if matches(OPCODE_IOCSR, OPCODE_IOCSR_LEN) {
        return emulate_iocsr(ctx, ins, vm_id, vcpu_id);
    }

    panic!(
        "Unhandled LoongArch GSPR instruction: pc={:#x}, badi={:#x}",
        get_guest_pc(ctx),
        ins
    );
}

pub fn handle_exception_sync(
    ctx: &mut LoongArchContextFrame,
    vm_id: VMId,
    vcpu_id: VCpuId,
    guest_timer_token: &mut Option<usize>,
) -> AxResult<AxVCpuExitReason> {
    let ecode = get_exception_code(ctx);
    let esubcode = get_exception_subcode(ctx);
    if SYNC_EXIT_LOGS.fetch_add(1, Ordering::Relaxed) < 32 {
        log::warn!(
            "LoongArch guest sync exit: ecode={:#x}, esubcode={:#x}, guest_pc={:#x}, \
             host_era={:#x}, gera={:#x}, badv={:#x}, badi={:#x}, guest_is={:#x}, \
             host_estat={:#x}, tlbrbadv={:#x}, tlbrera={:#x}",
            ecode,
            esubcode,
            get_guest_pc(ctx),
            ctx.host_era,
            ctx.gcsr_era,
            get_badv(ctx),
            get_badi(ctx),
            get_guest_interrupt_status(ctx),
            ctx.host_estat,
            ctx.host_tlbrbadv,
            ctx.host_tlbrera,
        );
    }

    log::debug!(
        "LoongArch guest sync exit: ecode={:#x}, esubcode={:#x}, guest_pc={:#x}, host_era={:#x}, \
         gera={:#x}, badv={:#x}, badi={:#x}, host_estat={:#x}, tlbrera={:#x}",
        ecode,
        esubcode,
        get_guest_pc(ctx),
        ctx.host_era,
        ctx.gcsr_era,
        get_badv(ctx),
        get_badi(ctx),
        ctx.host_estat,
        ctx.host_tlbrera
    );

    if is_host_tlb_refill(ctx) {
        let badv = get_badv(ctx);
        if should_inject_guest_virtual_fault(ctx, badv, true) {
            inject_guest_tlb_refill(ctx, badv);
            return Ok(AxVCpuExitReason::Nothing);
        }
        ctx.sepc = get_guest_pc(ctx);
        if NESTED_FAULT_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
            log::warn!(
                "LoongArch nested fault from host refill: badv={:#x}, gpa={:#x}, pc={:#x}, \
                 crmd={:#x}, eentry={:#x}, tlbrentry={:#x}, tlbrera={:#x}, host_estat={:#x}",
                badv,
                direct_map_guest_addr_to_gpa(badv),
                get_guest_pc(ctx),
                ctx.gcsr_crmd,
                ctx.gcsr_eentry,
                ctx.gcsr_tlbrentry,
                ctx.host_tlbrera,
                ctx.host_estat
            );
        }
        return Ok(AxVCpuExitReason::NestedPageFault {
            addr: GuestPhysAddr::from(direct_map_guest_addr_to_gpa(badv)),
            access_flags: get_refill_access_flags(ctx),
        });
    }

    if ecode == 0 && decode_interrupt_vector(get_guest_interrupt_status(ctx)).is_some() {
        return handle_exception_irq(ctx);
    }

    let result = match ecode {
        ECODE_HVC => {
            let nr = ctx.get_a0() as u64;
            let args = [
                ctx.get_a1() as u64,
                ctx.get_a2() as u64,
                ctx.get_a3() as u64,
                ctx.get_a4() as u64,
                ctx.get_a5() as u64,
                ctx.get_a6() as u64,
            ];
            advance_guest_pc(ctx);
            Ok(AxVCpuExitReason::Hypercall { nr, args })
        }
        ECODE_GSPR => Ok(emulate_gspr(ctx, vm_id, vcpu_id, guest_timer_token)),
        ECODE_PIL | ECODE_PIS | ECODE_PIF | ECODE_PME | ECODE_PNR | ECODE_PNX | ECODE_PPI => {
            let badv = get_badv(ctx);
            if should_inject_guest_virtual_fault(ctx, badv, false) {
                inject_guest_regular_exception(ctx, ecode, esubcode, badv);
                return Ok(AxVCpuExitReason::Nothing);
            }
            let mut access_flags = MappingFlags::empty();
            if matches!(ecode, ECODE_PIS | ECODE_PME) {
                access_flags |= MappingFlags::WRITE;
            } else if ecode == ECODE_PIF {
                access_flags |= MappingFlags::EXECUTE;
            } else {
                access_flags |= MappingFlags::READ;
            }
            if NESTED_FAULT_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
                log::warn!(
                    "LoongArch nested fault from regular exception: ecode={:#x}, badv={:#x}, \
                     gpa={:#x}, pc={:#x}, crmd={:#x}, eentry={:#x}, tlbrentry={:#x}, \
                     host_estat={:#x}",
                    ecode,
                    badv,
                    direct_map_guest_addr_to_gpa(badv),
                    get_guest_pc(ctx),
                    ctx.gcsr_crmd,
                    ctx.gcsr_eentry,
                    ctx.gcsr_tlbrentry,
                    ctx.host_estat
                );
            }
            Ok(AxVCpuExitReason::NestedPageFault {
                addr: GuestPhysAddr::from(direct_map_guest_addr_to_gpa(badv)),
                access_flags,
            })
        }
        // Per the LoongArch manuals and hvisor, ecode=0x8 is ADE:
        // esubcode=0 => ADEF (instruction fetch address exception),
        // esubcode=1 => ADEM (data access address exception).
        // It is not a TLB refill / nested page fault, so retrying the vCPU
        // would only spin forever on the same synchronous exception.
        ECODE_ADE => panic!(
            "LoongArch guest address exception: kind={}, sepc={:#x}, gera={:#x}, badv={:#x}, \
             badi={:#x}",
            match esubcode {
                ESUBCODE_ADEF => "ADEF",
                ESUBCODE_ADEM => "ADEM",
                _ => "ADE",
            },
            get_guest_pc(ctx),
            ctx.gcsr_era,
            get_badv(ctx),
            get_badi(ctx)
        ),
        ECODE_RSE => Ok(AxVCpuExitReason::Halt),
        _ => panic!(
            "Unhandled synchronous exception: ecode={:#x}, esubcode={:#x}, sepc={:#x}, \
             gera={:#x}, badv={:#x}, badi={:#x}",
            ecode,
            esubcode,
            get_guest_pc(ctx),
            ctx.gcsr_era,
            get_badv(ctx),
            get_badi(ctx)
        ),
    };
    // A host timer can be pending alongside a synchronous guest exit. It is a
    // host scheduling event here, not necessarily a guest timer interrupt.
    if get_guest_interrupt_status(ctx) & TIMER_BIT != 0 {
        ack_host_timer_interrupt();
    }
    result
}

pub fn handle_exception_irq(ctx: &mut LoongArchContextFrame) -> AxResult<AxVCpuExitReason> {
    let guest_is = get_guest_interrupt_status(ctx);
    let is = guest_is;

    if let Some(vector) = decode_interrupt_vector(is) {
        log::trace!(
            "LoongArch guest irq exit: vector={}, guest_is={:#x}, sepc={:#x}, gera={:#x}",
            vector,
            guest_is,
            get_guest_pc(ctx),
            ctx.gcsr_era
        );

        // Host timer exits drive the scheduler and Axvisor timer wheel. Do not
        // translate every host tick into a guest timer interrupt: doing so can
        // interrupt Linux before it has initialized its guest exception state.
        if vector == INT_TIMER {
            ack_host_timer_interrupt();
        }

        return Ok(AxVCpuExitReason::ExternalInterrupt {
            vector: vector as u64,
        });
    }

    log::trace!(
        "LoongArch guest irq exit with unknown status: guest_is={:#x}, saved_estat={:#x}, \
         sepc={:#x}, gera={:#x}",
        guest_is,
        ctx.host_estat,
        get_guest_pc(ctx),
        ctx.gcsr_era
    );
    Ok(AxVCpuExitReason::Nothing)
}

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(
    include_str!("exception.S"),
    ctx_size = const core::mem::size_of::<LoongArchContextFrame>(),
    host_pgdl = const offset_of!(LoongArchContextFrame, host_pgdl),
    host_pgdh = const offset_of!(LoongArchContextFrame, host_pgdh),
    host_pwcl = const offset_of!(LoongArchContextFrame, host_pwcl),
    host_pwch = const offset_of!(LoongArchContextFrame, host_pwch),
    host_stlbps = const offset_of!(LoongArchContextFrame, host_stlbps),
    host_tlbrentry = const offset_of!(LoongArchContextFrame, host_tlbrentry),
    host_asid = const offset_of!(LoongArchContextFrame, host_asid),
    host_eentry = const offset_of!(LoongArchContextFrame, host_eentry),
    host_ecfg = const offset_of!(LoongArchContextFrame, host_ecfg),
    guest_tlbrentry = const offset_of!(LoongArchContextFrame, guest_tlbrentry),
    guest_eentry = const offset_of!(LoongArchContextFrame, guest_eentry),
);

#[cfg(target_arch = "loongarch64")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
unsafe extern "C" fn vmexit_trampoline() -> ! {
    core::arch::naked_asm!(
        "addi.d $t0, $sp, {ctx_size}",
        "ld.d $t1, $t0, 0",
        "move $sp, $t1",
        "ld.d $ra, $sp, 0",
        "ld.d $s0, $sp, 8",
        "ld.d $s1, $sp, 16",
        "ld.d $s2, $sp, 24",
        "ld.d $s3, $sp, 32",
        "ld.d $s4, $sp, 40",
        "ld.d $s5, $sp, 48",
        "ld.d $s6, $sp, 56",
        "ld.d $s7, $sp, 64",
        "ld.d $s8, $sp, 72",
        "ld.d $fp, $sp, 80",
        "ld.d $tp, $sp, 88",
        "ld.d $r21, $sp, 96",
        "addi.d $sp, $sp, 14 * 8",
        "jr $ra",
        ctx_size = const core::mem::size_of::<LoongArchContextFrame>(),
    )
}

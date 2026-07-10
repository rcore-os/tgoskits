use crate::context_frame::LoongArchContextFrame;

pub(crate) const ECODE_HVC: usize = 0x17;
pub(crate) const ECODE_GSPR: usize = 0x16;
pub(crate) const ECODE_PIL: usize = 0x1;
pub(crate) const ECODE_PIS: usize = 0x2;
pub(crate) const ECODE_PIF: usize = 0x3;
pub(crate) const ECODE_PME: usize = 0x4;
pub(crate) const ECODE_PNR: usize = 0x5;
pub(crate) const ECODE_PNX: usize = 0x6;
pub(crate) const ECODE_PPI: usize = 0x7;
pub(crate) const ECODE_ADE: usize = 0x8;
pub(crate) const ESUBCODE_ADEF: usize = 0x0;
pub(crate) const ESUBCODE_ADEM: usize = 0x1;
pub(crate) const ECODE_RSE: usize = 0x10;
pub(crate) const LOCAL_INTERRUPT_MASK: usize = (1 << 13) - 1;
pub(crate) const INT_HWI0: usize = 2;
pub(crate) const INT_HWI7: usize = 9;
pub(crate) const INT_TIMER: usize = 11;
pub(crate) const INT_IPI: usize = 12;
pub(crate) const TIMER_BIT: usize = 1 << INT_TIMER;
pub(crate) const IPI_BIT: usize = 1 << INT_IPI;
pub(crate) const HWI_MASK: usize = ((1 << (INT_HWI7 + 1)) - 1) & !((1 << INT_HWI0) - 1);

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

pub(crate) fn get_exception_code(ctx: &LoongArchContextFrame) -> usize {
    (ctx.host_estat >> 16) & 0x3f
}

pub(crate) fn get_exception_subcode(ctx: &LoongArchContextFrame) -> usize {
    (ctx.host_estat >> 22) & 0x1ff
}

pub(crate) fn is_host_tlb_refill(ctx: &LoongArchContextFrame) -> bool {
    ctx.host_tlbrera & 0x1 != 0
}

pub(crate) fn get_guest_pc(ctx: &LoongArchContextFrame) -> usize {
    ctx.guest_exception_pc()
}

pub(crate) fn get_badv(ctx: &LoongArchContextFrame) -> usize {
    if ctx.host_tlbrera & 0x1 != 0 {
        ctx.host_tlbrbadv
    } else {
        ctx.host_badv
    }
}

pub(crate) fn get_badi(ctx: &LoongArchContextFrame) -> usize {
    ctx.host_badi
}

pub(crate) fn current_badi() -> usize {
    unsafe { crate::registers::csr_read::<0x8>() }
}

pub(crate) fn get_guest_interrupt_status(ctx: &LoongArchContextFrame) -> usize {
    ctx.host_estat & LOCAL_INTERRUPT_MASK
}

pub(crate) fn decode_interrupt_vector(is: usize) -> Option<usize> {
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

pub(crate) fn extract_field(value: usize, offset: usize, width: usize) -> usize {
    (value >> offset) & ((1usize << width) - 1)
}

pub(crate) fn advance_guest_pc(ctx: &mut LoongArchContextFrame) {
    ctx.advance_guest_pc();
}

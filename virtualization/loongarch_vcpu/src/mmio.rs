use axvcpu::{AccessWidth, AxVCpuExitReason, GuestPhysAddr, MappingFlags};

use crate::context_frame::LoongArchContextFrame;

const INSN_RJ_SHIFT: usize = 5;
const INSN_REG_MASK: usize = 0x1f;
const MEMORY_ACCESS_OP_SHIFT: usize = 22;
const MEMORY_ACCESS_OP_MASK: usize = 0x3ff;
const MEMORY_ACCESS_IMM_SHIFT: usize = 10;
const MEMORY_ACCESS_IMM_MASK: usize = 0xfff;
const MEMORY_ACCESS_RD_SHIFT: usize = 0;
const LD_B_OP: usize = 0x0a0;
const LD_H_OP: usize = 0x0a1;
const LD_W_OP: usize = 0x0a2;
const LD_D_OP: usize = 0x0a3;
const ST_B_OP: usize = 0x0a4;
const ST_H_OP: usize = 0x0a5;
const ST_W_OP: usize = 0x0a6;
const ST_D_OP: usize = 0x0a7;
const LD_BU_OP: usize = 0x0a8;
const LD_HU_OP: usize = 0x0a9;
const LD_WU_OP: usize = 0x0aa;
const LDPTR_W_PREFIX: usize = 0x24;
const STPTR_W_PREFIX: usize = 0x25;
const LDPTR_D_PREFIX: usize = 0x26;
const STPTR_D_PREFIX: usize = 0x27;
const PTR_OP_SHIFT: usize = 24;
const PTR_OP_MASK: usize = 0xff;
const PTR_IMM_SHIFT: usize = 10;
const PTR_IMM_MASK: usize = 0x3fff;
const INDEXED_ACCESS_OP_SHIFT: usize = 15;
const INDEXED_ACCESS_OP_MASK: usize = 0x1ffff;
const LDX_BU_OP: usize = 0x7000;
const LDX_HU_OP: usize = 0x7008;
const LDX_W_OP: usize = 0x7010;
const LDX_D_OP: usize = 0x7018;
const STX_B_OP: usize = 0x7020;
const STX_H_OP: usize = 0x7028;
const STX_W_OP: usize = 0x7030;
const STX_D_OP: usize = 0x7038;
const LDX_B_OP: usize = 0x7040;
const LDX_H_OP: usize = 0x7048;
const LDX_WU_OP: usize = 0x7050;

pub fn decode_mmio_fault(
    ctx: &mut LoongArchContextFrame,
    insn: usize,
    fault_addr: GuestPhysAddr,
    access_flags: MappingFlags,
) -> Option<AxVCpuExitReason> {
    let access_flags = refine_access_flags_from_insn(insn, access_flags);
    let fault_addr = fault_addr.as_usize();
    let op = (insn >> MEMORY_ACCESS_OP_SHIFT) & MEMORY_ACCESS_OP_MASK;
    let rd = (insn >> MEMORY_ACCESS_RD_SHIFT) & INSN_REG_MASK;
    let exit_reason = match op {
        LD_B_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Byte,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: true,
        },
        LD_H_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Word,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: true,
        },
        LD_W_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Dword,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: true,
        },
        LD_D_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Qword,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: false,
        },
        LD_BU_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Byte,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: false,
        },
        LD_HU_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Word,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: false,
        },
        LD_WU_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Dword,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: false,
        },
        ST_B_OP if access_flags.contains(MappingFlags::WRITE) => AxVCpuExitReason::MmioWrite {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Byte,
            data: ctx.gpr(rd) as u64,
        },
        ST_H_OP if access_flags.contains(MappingFlags::WRITE) => AxVCpuExitReason::MmioWrite {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Word,
            data: ctx.gpr(rd) as u64,
        },
        ST_W_OP if access_flags.contains(MappingFlags::WRITE) => AxVCpuExitReason::MmioWrite {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Dword,
            data: ctx.gpr(rd) as u64,
        },
        ST_D_OP if access_flags.contains(MappingFlags::WRITE) => AxVCpuExitReason::MmioWrite {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Qword,
            data: ctx.gpr(rd) as u64,
        },
        _ => {
            return decode_ptr_mmio_fault(ctx, insn, fault_addr, access_flags)
                .or_else(|| decode_indexed_mmio_fault(ctx, insn, fault_addr, access_flags));
        }
    };

    ctx.advance_guest_pc();
    Some(exit_reason)
}

fn refine_access_flags_from_insn(insn: usize, fallback: MappingFlags) -> MappingFlags {
    if is_load_insn(insn) {
        MappingFlags::READ
    } else if is_store_insn(insn) {
        MappingFlags::WRITE
    } else {
        fallback
    }
}

fn is_load_insn(insn: usize) -> bool {
    let op = (insn >> MEMORY_ACCESS_OP_SHIFT) & MEMORY_ACCESS_OP_MASK;
    matches!(
        op,
        LD_B_OP | LD_H_OP | LD_W_OP | LD_D_OP | LD_BU_OP | LD_HU_OP | LD_WU_OP
    ) || matches!(
        (insn >> PTR_OP_SHIFT) & PTR_OP_MASK,
        LDPTR_W_PREFIX | LDPTR_D_PREFIX
    ) || matches!(
        (insn >> INDEXED_ACCESS_OP_SHIFT) & INDEXED_ACCESS_OP_MASK,
        LDX_B_OP | LDX_H_OP | LDX_W_OP | LDX_D_OP | LDX_BU_OP | LDX_HU_OP | LDX_WU_OP
    )
}

fn is_store_insn(insn: usize) -> bool {
    let op = (insn >> MEMORY_ACCESS_OP_SHIFT) & MEMORY_ACCESS_OP_MASK;
    matches!(op, ST_B_OP | ST_H_OP | ST_W_OP | ST_D_OP)
        || matches!(
            (insn >> PTR_OP_SHIFT) & PTR_OP_MASK,
            STPTR_W_PREFIX | STPTR_D_PREFIX
        )
        || matches!(
            (insn >> INDEXED_ACCESS_OP_SHIFT) & INDEXED_ACCESS_OP_MASK,
            STX_B_OP | STX_H_OP | STX_W_OP | STX_D_OP
        )
}

pub fn describe_mmio_fault(
    ctx: &LoongArchContextFrame,
    insn: usize,
) -> (usize, usize, usize, usize) {
    let rj = (insn >> INSN_RJ_SHIFT) & INSN_REG_MASK;
    let rj_value = ctx.gpr(rj);
    let imm12 = sign_extend_12((insn >> MEMORY_ACCESS_IMM_SHIFT) & MEMORY_ACCESS_IMM_MASK);
    let normal_addr = direct_mapped_guest_phys_addr(rj_value.wrapping_add(imm12));
    let imm14 = sign_extend_14((insn >> PTR_IMM_SHIFT) & PTR_IMM_MASK).wrapping_shl(2);
    let ptr_addr = direct_mapped_guest_phys_addr(rj_value.wrapping_add(imm14));
    (rj, rj_value, normal_addr, ptr_addr)
}

fn decode_ptr_mmio_fault(
    ctx: &mut LoongArchContextFrame,
    insn: usize,
    fault_addr: usize,
    access_flags: MappingFlags,
) -> Option<AxVCpuExitReason> {
    let op = (insn >> PTR_OP_SHIFT) & PTR_OP_MASK;
    let rd = (insn >> MEMORY_ACCESS_RD_SHIFT) & INSN_REG_MASK;
    let exit_reason = match op {
        LDPTR_W_PREFIX if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Dword,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: true,
        },
        LDPTR_D_PREFIX if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Qword,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: false,
        },
        STPTR_W_PREFIX if access_flags.contains(MappingFlags::WRITE) => {
            AxVCpuExitReason::MmioWrite {
                addr: GuestPhysAddr::from(fault_addr),
                width: AccessWidth::Dword,
                data: ctx.gpr(rd) as u64,
            }
        }
        STPTR_D_PREFIX if access_flags.contains(MappingFlags::WRITE) => {
            AxVCpuExitReason::MmioWrite {
                addr: GuestPhysAddr::from(fault_addr),
                width: AccessWidth::Qword,
                data: ctx.gpr(rd) as u64,
            }
        }
        _ => return decode_indexed_mmio_fault(ctx, insn, fault_addr, access_flags),
    };

    ctx.advance_guest_pc();
    Some(exit_reason)
}

fn decode_indexed_mmio_fault(
    ctx: &mut LoongArchContextFrame,
    insn: usize,
    fault_addr: usize,
    access_flags: MappingFlags,
) -> Option<AxVCpuExitReason> {
    let op = (insn >> INDEXED_ACCESS_OP_SHIFT) & INDEXED_ACCESS_OP_MASK;
    let rd = (insn >> MEMORY_ACCESS_RD_SHIFT) & INSN_REG_MASK;

    let exit_reason = match op {
        LDX_B_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Byte,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: true,
        },
        LDX_H_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Word,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: true,
        },
        LDX_W_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Dword,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: true,
        },
        LDX_D_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Qword,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: false,
        },
        LDX_BU_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Byte,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: false,
        },
        LDX_HU_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Word,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: false,
        },
        LDX_WU_OP if access_flags.contains(MappingFlags::READ) => AxVCpuExitReason::MmioRead {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Dword,
            reg: rd,
            reg_width: AccessWidth::Qword,
            signed_ext: false,
        },
        STX_B_OP if access_flags.contains(MappingFlags::WRITE) => AxVCpuExitReason::MmioWrite {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Byte,
            data: ctx.gpr(rd) as u64,
        },
        STX_H_OP if access_flags.contains(MappingFlags::WRITE) => AxVCpuExitReason::MmioWrite {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Word,
            data: ctx.gpr(rd) as u64,
        },
        STX_W_OP if access_flags.contains(MappingFlags::WRITE) => AxVCpuExitReason::MmioWrite {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Dword,
            data: ctx.gpr(rd) as u64,
        },
        STX_D_OP if access_flags.contains(MappingFlags::WRITE) => AxVCpuExitReason::MmioWrite {
            addr: GuestPhysAddr::from(fault_addr),
            width: AccessWidth::Qword,
            data: ctx.gpr(rd) as u64,
        },
        _ => return None,
    };

    ctx.advance_guest_pc();
    Some(exit_reason)
}

fn sign_extend_12(value: usize) -> usize {
    ((value << (usize::BITS as usize - 12)) as isize >> (usize::BITS as usize - 12)) as usize
}

fn sign_extend_14(value: usize) -> usize {
    ((value << (usize::BITS as usize - 14)) as isize >> (usize::BITS as usize - 14)) as usize
}

fn direct_mapped_guest_phys_addr(addr: usize) -> usize {
    const DMW_PREFIX_MASK: usize = 0xf000_0000_0000_0000;
    const DMW0_PREFIX: usize = 0x8000_0000_0000_0000;
    const DMW1_PREFIX: usize = 0x9000_0000_0000_0000;
    const DMW_PHYS_MASK: usize = 0x0000_ffff_ffff_ffff;

    match addr & DMW_PREFIX_MASK {
        DMW0_PREFIX | DMW1_PREFIX => addr & DMW_PHYS_MASK,
        _ => addr,
    }
}

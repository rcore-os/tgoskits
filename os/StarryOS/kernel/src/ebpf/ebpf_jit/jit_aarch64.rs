use super::{
    BpfInsn, HelperFn, JitBackend, JitBuffer,
    bpf_insn::{
        BPF_ADD, BPF_ALU, BPF_ALU64, BPF_AND, BPF_ARSH, BPF_B, BPF_DIV, BPF_DW, BPF_EXIT, BPF_H,
        BPF_JA, BPF_JEQ, BPF_JGE, BPF_JGT, BPF_JLE, BPF_JLT, BPF_JMP, BPF_JMP32, BPF_JNE, BPF_JSET,
        BPF_JSGE, BPF_JSGT, BPF_JSLE, BPF_JSLT, BPF_LD, BPF_LDX, BPF_LSH, BPF_MEM, BPF_MOD,
        BPF_MOV, BPF_MUL, BPF_NEG, BPF_OR, BPF_RSH, BPF_ST, BPF_STX, BPF_SUB, BPF_W, BPF_X,
        BPF_XOR,
    },
};

const AA_X0: u32 = 0;
const AA_X1: u32 = 1;
const AA_X2: u32 = 2;
const AA_X3: u32 = 3;
const AA_X4: u32 = 4;
const AA_X5: u32 = 5;
const AA_X7: u32 = 7;
const AA_X9: u32 = 9;
const AA_X15: u32 = 15;
const AA_X16: u32 = 16;
const AA_X17: u32 = 17;
const AA_SP: u32 = 31;
const AA_X29: u32 = 29;
const AA_LR: u32 = 30;

fn bpf_to_aa(r: u8) -> u32 {
    match r {
        0 => AA_X0,
        1 => AA_X1,
        2 => AA_X2,
        3 => AA_X3,
        4 => AA_X4,
        5 => AA_X5,
        6 => AA_X7,
        7 => AA_X9,
        8 => AA_X15,
        9 => AA_X16,
        10 => AA_X29,
        _ => AA_X0,
    }
}

fn emit_add(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x8B000000 | (rm << 16) | (rn << 5) | rd);
}

fn emit_addw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x0B000000 | (rm << 16) | (rn << 5) | rd);
}

fn emit_sub(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0xCB000000 | (rm << 16) | (rn << 5) | rd);
}

fn emit_subw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x4B000000 | (rm << 16) | (rn << 5) | rd);
}

fn emit_addi(buf: &mut JitBuffer, rd: u32, rn: u32, imm: i32) {
    if imm >= 0 {
        let imm12 = (imm as u32) & 0xFFF;
        buf.emit_u32(0x91000000 | (imm12 << 10) | (rn << 5) | rd);
    } else {
        emit_subi(buf, rd, rn, -imm);
    }
}

fn emit_addiw(buf: &mut JitBuffer, rd: u32, rn: u32, imm: i32) {
    let imm12 = (imm as u32) & 0xFFF;
    buf.emit_u32(0x11000000 | (imm12 << 10) | (rn << 5) | rd);
}

fn emit_subi(buf: &mut JitBuffer, rd: u32, rn: u32, imm: i32) {
    let imm12 = (imm as u32) & 0xFFF;
    buf.emit_u32(0xD1000000 | (imm12 << 10) | (rn << 5) | rd);
}

fn emit_and(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x8A000000 | (rm << 16) | (rn << 5) | rd);
}

fn emit_andw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x0A000000 | (rm << 16) | (rn << 5) | rd);
}

fn emit_or(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x8A200000 | (rm << 16) | (rn << 5) | rd);
}

fn emit_orw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x0A200000 | (rm << 16) | (rn << 5) | rd);
}

fn emit_xor(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x8A400000 | (rm << 16) | (rn << 5) | rd);
}

fn emit_xorw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x0A400000 | (rm << 16) | (rn << 5) | rd);
}

fn emit_mul(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x9B007C00 | (rm << 16) | (rn << 5) | rd);
}

fn emit_mulw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x1B007C00 | (rm << 16) | (rn << 5) | rd);
}

fn emit_udiv(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x9AC00C00 | (rm << 16) | (rn << 5) | rd);
}

fn emit_udivw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x1AC00C00 | (rm << 16) | (rn << 5) | rd);
}

fn emit_msub(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32, ra: u32) {
    buf.emit_u32(0x9B00FC00 | (rm << 16) | (ra << 10) | (rn << 5) | rd);
}

fn emit_msubw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32, ra: u32) {
    buf.emit_u32(0x1B00FC00 | (rm << 16) | (ra << 10) | (rn << 5) | rd);
}

fn emit_lsl(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x9AC02000 | (rm << 16) | (rn << 5) | rd);
}

fn emit_lslw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x1AC02000 | (rm << 16) | (rn << 5) | rd);
}

fn emit_lsr(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x9AC02400 | (rm << 16) | (rn << 5) | rd);
}

fn emit_lsrw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x1AC02400 | (rm << 16) | (rn << 5) | rd);
}

fn emit_asr(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x9AC02800 | (rm << 16) | (rn << 5) | rd);
}

fn emit_asrw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x1AC02800 | (rm << 16) | (rn << 5) | rd);
}

fn emit_lsl_imm(buf: &mut JitBuffer, rd: u32, rn: u32, imm: u32) {
    buf.emit_u32(0xD3400000 | (((64 - imm) & 63) << 16) | (rn << 5) | rd);
}

fn emit_lsl_immw(buf: &mut JitBuffer, rd: u32, rn: u32, imm: u32) {
    buf.emit_u32(0x53000000 | (((32 - imm) & 31) << 16) | (rn << 5) | rd);
}

fn emit_lsr_imm(buf: &mut JitBuffer, rd: u32, rn: u32, imm: u32) {
    buf.emit_u32(0xD340FC00 | ((imm & 63) << 16) | (rn << 5) | rd);
}

fn emit_lsr_immw(buf: &mut JitBuffer, rd: u32, rn: u32, imm: u32) {
    buf.emit_u32(0x53007C00 | ((imm & 31) << 16) | (rn << 5) | rd);
}

fn emit_asr_imm(buf: &mut JitBuffer, rd: u32, rn: u32, imm: u32) {
    buf.emit_u32(0x9340FC00 | ((imm & 63) << 16) | (rn << 5) | rd);
}

fn emit_asr_immw(buf: &mut JitBuffer, rd: u32, rn: u32, imm: u32) {
    buf.emit_u32(0x13007C00 | ((imm & 31) << 16) | (rn << 5) | rd);
}

fn emit_neg(buf: &mut JitBuffer, rd: u32, rn: u32) {
    emit_sub(buf, rd, 31, rn);
}

fn emit_negw(buf: &mut JitBuffer, rd: u32, rn: u32) {
    emit_subw(buf, rd, 31, rn);
}

fn emit_mov(buf: &mut JitBuffer, rd: u32, rn: u32) {
    buf.emit_u32(0xAA0003E0 | (rn << 16) | rd);
}

fn emit_movw(buf: &mut JitBuffer, rd: u32, rn: u32) {
    buf.emit_u32(0x2A0003E0 | (rn << 16) | rd);
}

fn emit_movz16(buf: &mut JitBuffer, rd: u32, imm: u16, shift: u32) {
    buf.emit_u32(0x52800000 | (shift << 21) | ((imm as u32) << 5) | rd);
}

fn emit_movk16(buf: &mut JitBuffer, rd: u32, imm: u16, shift: u32) {
    buf.emit_u32(0x72800000 | (shift << 21) | ((imm as u32) << 5) | rd);
}

fn emit_load_imm64(buf: &mut JitBuffer, rd: u32, val: u64) {
    emit_movz16(buf, rd, (val & 0xFFFF) as u16, 0);
    emit_movk16(buf, rd, ((val >> 16) & 0xFFFF) as u16, 1);
    emit_movk16(buf, rd, ((val >> 32) & 0xFFFF) as u16, 2);
    emit_movk16(buf, rd, ((val >> 48) & 0xFFFF) as u16, 3);
}

fn emit_load_imm32(buf: &mut JitBuffer, rd: u32, val: i32) {
    let v = val as u32;
    emit_movz16(buf, rd, (v & 0xFFFF) as u16, 0);
    emit_movk16(buf, rd, ((v >> 16) & 0xFFFF) as u16, 1);
}

fn emit_str(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    let imm9 = (off << 2) as u32 & 0x1FFC;
    buf.emit_u32(0xF9000000 | imm9 | (rn << 5) | rt);
}

fn emit_strw(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    let imm9 = (off << 2) as u32 & 0x1FFC;
    buf.emit_u32(0xB9000000 | imm9 | (rn << 5) | rt);
}

fn emit_strh(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    let imm9 = (off << 1) as u32 & 0x1FFE;
    buf.emit_u32(0x79000000 | imm9 | (rn << 5) | rt);
}

fn emit_strb(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    let imm9 = (off) as u32 & 0xFFF;
    buf.emit_u32(0x39000000 | imm9 | (rn << 5) | rt);
}

fn emit_ldr(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    let imm9 = (off << 2) as u32 & 0x1FFC;
    buf.emit_u32(0xF9400000 | imm9 | (rn << 5) | rt);
}

fn emit_ldrw(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    let imm9 = (off << 2) as u32 & 0x1FFC;
    buf.emit_u32(0xB9400000 | imm9 | (rn << 5) | rt);
}

fn emit_ldrh(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    let imm9 = (off << 1) as u32 & 0x1FFE;
    buf.emit_u32(0x79400000 | imm9 | (rn << 5) | rt);
}

fn emit_ldrb(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    let imm9 = (off) as u32 & 0xFFF;
    buf.emit_u32(0x39400000 | imm9 | (rn << 5) | rt);
}

fn emit_stp(buf: &mut JitBuffer, rt1: u32, rt2: u32, rn: u32, off: i32) {
    let imm7 = ((off as i64) << 3) as u32 & 0x1FF8;
    buf.emit_u32(0xA9000000 | imm7 | (rt2 << 10) | (rn << 5) | rt1);
}

fn emit_ldp(buf: &mut JitBuffer, rt1: u32, rt2: u32, rn: u32, off: i32) {
    let imm7 = ((off as i64) << 3) as u32 & 0x1FF8;
    buf.emit_u32(0xA9400000 | imm7 | (rt2 << 10) | (rn << 5) | rt1);
}

fn emit_cmp(buf: &mut JitBuffer, rn: u32, rm: u32) {
    emit_sub(buf, 31, rn, rm);
}

fn emit_cmpw(buf: &mut JitBuffer, rn: u32, rm: u32) {
    emit_subw(buf, 31, rn, rm);
}

fn emit_cbz(buf: &mut JitBuffer, rt: u32, off: i32) {
    let imm19 = ((off as i64) >> 2) as u32 & 0x7FFFF;
    buf.emit_u32(0x34000000 | (imm19 << 5) | rt);
}

fn emit_cbnz(buf: &mut JitBuffer, rt: u32, off: i32) {
    let imm19 = ((off as i64) >> 2) as u32 & 0x7FFFF;
    buf.emit_u32(0x35000000 | (imm19 << 5) | rt);
}

fn emit_bcond(buf: &mut JitBuffer, cond: u32, off: i32) {
    let imm19 = ((off as i64) >> 2) as u32 & 0x7FFFF;
    buf.emit_u32(0x54000000 | (imm19 << 5) | cond);
}

fn emit_b(buf: &mut JitBuffer, off: i32) {
    let imm26 = ((off as i64) >> 2) as u32 & 0x3FFFFFF;
    buf.emit_u32(0x14000000 | imm26);
}

fn emit_blr(buf: &mut JitBuffer, rn: u32) {
    buf.emit_u32(0xD63F0000 | (rn << 5));
}

fn emit_ret(buf: &mut JitBuffer) {
    buf.emit_u32(0xD65F03C0);
}

pub(crate) struct Aarch64Backend;

const BPF_STACK_SIZE: usize = 512;
const FRAME_SIZE: usize = BPF_STACK_SIZE + 8 * 5;

impl JitBackend for Aarch64Backend {
    fn emit_prologue(buf: &mut JitBuffer) -> usize {
        emit_subi(buf, AA_SP, AA_SP, FRAME_SIZE as i32);
        emit_stp(buf, AA_X29, AA_X7, AA_SP, BPF_STACK_SIZE as i32);
        emit_stp(buf, AA_X9, AA_X15, AA_SP, (BPF_STACK_SIZE + 16) as i32);
        emit_str(buf, AA_X16, AA_SP, (BPF_STACK_SIZE + 32) as i32);
        emit_addi(buf, AA_X29, AA_SP, FRAME_SIZE as i32);
        emit_mov(buf, AA_X1, AA_X0);
        buf.offset()
    }

    fn emit_epilogue(buf: &mut JitBuffer) {
        emit_ldp(buf, AA_X29, AA_X7, AA_SP, BPF_STACK_SIZE as i32);
        emit_ldp(buf, AA_X9, AA_X15, AA_SP, (BPF_STACK_SIZE + 16) as i32);
        emit_ldr(buf, AA_X16, AA_SP, (BPF_STACK_SIZE + 32) as i32);
        emit_addi(buf, AA_SP, AA_SP, FRAME_SIZE as i32);
        emit_ret(buf);
    }

    fn emit_alu(buf: &mut JitBuffer, insn: &BpfInsn, is_64: bool) {
        let dst = bpf_to_aa(insn.dst_reg());
        let use_imm = (insn.code & BPF_X) == 0;
        let src = if use_imm {
            AA_X17
        } else {
            bpf_to_aa(insn.src_reg())
        };

        if use_imm {
            if is_64 {
                emit_load_imm64(buf, AA_X17, insn.imm as u64);
            } else {
                emit_load_imm32(buf, AA_X17, insn.imm);
            }
        }

        match insn.alu_op() {
            BPF_ADD => {
                if is_64 {
                    emit_add(buf, dst, dst, src);
                } else {
                    emit_addw(buf, dst, dst, src);
                }
            }
            BPF_SUB => {
                if is_64 {
                    emit_sub(buf, dst, dst, src);
                } else {
                    emit_subw(buf, dst, dst, src);
                }
            }
            BPF_MUL => {
                if is_64 {
                    emit_mul(buf, dst, dst, src);
                } else {
                    emit_mulw(buf, dst, dst, src);
                }
            }
            BPF_DIV => {
                if is_64 {
                    emit_cbz(buf, src, 8);
                    emit_udiv(buf, dst, dst, src);
                    emit_b(buf, 8);
                    emit_movz16(buf, dst, 0, 0);
                } else {
                    emit_cbz(buf, src, 8);
                    emit_udivw(buf, dst, dst, src);
                    emit_b(buf, 8);
                    emit_movz16(buf, dst, 0, 0);
                }
            }
            BPF_OR => {
                if is_64 {
                    emit_or(buf, dst, dst, src);
                } else {
                    emit_orw(buf, dst, dst, src);
                }
            }
            BPF_AND => {
                if is_64 {
                    emit_and(buf, dst, dst, src);
                } else {
                    emit_andw(buf, dst, dst, src);
                }
            }
            BPF_LSH => {
                if use_imm {
                    let shamt = (insn.imm as u32) & (if is_64 { 63 } else { 31 });
                    if is_64 {
                        emit_lsl_imm(buf, dst, dst, shamt);
                    } else {
                        emit_lsl_immw(buf, dst, dst, shamt);
                    }
                } else if is_64 {
                    emit_lsl(buf, dst, dst, src);
                } else {
                    emit_lslw(buf, dst, dst, src);
                }
            }
            BPF_RSH => {
                if use_imm {
                    let shamt = (insn.imm as u32) & (if is_64 { 63 } else { 31 });
                    if is_64 {
                        emit_lsr_imm(buf, dst, dst, shamt);
                    } else {
                        emit_lsr_immw(buf, dst, dst, shamt);
                    }
                } else if is_64 {
                    emit_lsr(buf, dst, dst, src);
                } else {
                    emit_lsrw(buf, dst, dst, src);
                }
            }
            BPF_NEG => {
                if is_64 {
                    emit_neg(buf, dst, dst);
                } else {
                    emit_negw(buf, dst, dst);
                }
            }
            BPF_MOD => {
                if is_64 {
                    emit_cbz(buf, src, 12);
                    emit_udiv(buf, AA_X17, dst, src);
                    emit_msub(buf, dst, AA_X17, src, dst);
                    emit_b(buf, 8);
                    emit_movz16(buf, dst, 0, 0);
                } else {
                    emit_cbz(buf, src, 12);
                    emit_udivw(buf, AA_X17, dst, src);
                    emit_msubw(buf, dst, AA_X17, src, dst);
                    emit_b(buf, 8);
                    emit_movz16(buf, dst, 0, 0);
                }
            }
            BPF_XOR => {
                if is_64 {
                    emit_xor(buf, dst, dst, src);
                } else {
                    emit_xorw(buf, dst, dst, src);
                }
            }
            BPF_MOV => {
                if use_imm {
                    if is_64 {
                        emit_load_imm64(buf, dst, insn.imm as u64);
                    } else {
                        emit_load_imm32(buf, dst, insn.imm);
                    }
                } else if is_64 {
                    emit_mov(buf, dst, src);
                } else {
                    emit_movw(buf, dst, src);
                }
            }
            BPF_ARSH => {
                if use_imm {
                    let shamt = (insn.imm as u32) & (if is_64 { 63 } else { 31 });
                    if is_64 {
                        emit_asr_imm(buf, dst, dst, shamt);
                    } else {
                        emit_asr_immw(buf, dst, dst, shamt);
                    }
                } else if is_64 {
                    emit_asr(buf, dst, dst, src);
                } else {
                    emit_asrw(buf, dst, dst, src);
                }
            }
            _ => {}
        }
    }

    fn emit_jmp(buf: &mut JitBuffer, insn: &BpfInsn, offsets: &[usize], pc: usize, is_64: bool) {
        let op = insn.code & 0xf0;

        if insn.code == (BPF_JMP | BPF_JA) || insn.code == (BPF_JMP32 | BPF_JA) {
            let target_pc = (pc as isize + 1 + insn.off as isize) as usize;
            if target_pc < offsets.len() {
                let off = offsets[target_pc] as isize - buf.offset() as isize;
                emit_b(buf, off as i32);
            }
            return;
        }

        if op == BPF_EXIT {
            let off = buf.offset() as isize - offsets[0] as isize;
            emit_b(buf, -(off as i32));
            return;
        }

        let dst = bpf_to_aa(insn.dst_reg());
        let use_imm = (insn.code & BPF_X) == 0;
        let src = if use_imm {
            AA_X17
        } else {
            bpf_to_aa(insn.src_reg())
        };

        if use_imm {
            if is_64 {
                emit_load_imm64(buf, AA_X17, insn.imm as u64);
            } else {
                emit_load_imm32(buf, AA_X17, insn.imm);
            }
        }

        if is_64 {
            emit_cmp(buf, dst, src);
        } else {
            emit_cmpw(buf, dst, src);
        }

        let target_pc = (pc as isize + 1 + insn.off as isize) as usize;
        let target_off = if target_pc < offsets.len() {
            (offsets[target_pc] as isize - buf.offset() as isize) as i32
        } else {
            0
        };

        match op {
            BPF_JEQ => emit_bcond(buf, 0, target_off),
            BPF_JGT => emit_bcond(buf, 8, target_off),
            BPF_JGE => emit_bcond(buf, 2, target_off),
            BPF_JSET => {
                if is_64 {
                    emit_and(buf, AA_X17, dst, src);
                } else {
                    emit_andw(buf, AA_X17, dst, src);
                }
                emit_cbnz(buf, AA_X17, target_off);
            }
            BPF_JNE => emit_bcond(buf, 1, target_off),
            BPF_JSGT => emit_bcond(buf, 0xC, target_off),
            BPF_JSGE => emit_bcond(buf, 0xA, target_off),
            BPF_JLT => emit_bcond(buf, 3, target_off),
            BPF_JLE => emit_bcond(buf, 9, target_off),
            BPF_JSLT => emit_bcond(buf, 0xB, target_off),
            BPF_JSLE => emit_bcond(buf, 0xD, target_off),
            _ => {}
        }
    }

    fn emit_st(buf: &mut JitBuffer, insn: &BpfInsn) {
        if insn.mode() != BPF_MEM {
            return;
        }
        let off = insn.off as i32;
        if insn.size() == BPF_DW {
            emit_load_imm64(buf, AA_X17, insn.imm as u64);
        } else {
            emit_load_imm32(buf, AA_X17, insn.imm);
        }
        emit_addi(buf, AA_X16, AA_X29, off);
        match insn.size() {
            BPF_B => emit_strb(buf, AA_X17, AA_X16, 0),
            BPF_H => emit_strh(buf, AA_X17, AA_X16, 0),
            BPF_W => emit_strw(buf, AA_X17, AA_X16, 0),
            BPF_DW => emit_str(buf, AA_X17, AA_X16, 0),
            _ => {}
        }
    }

    fn emit_stx(buf: &mut JitBuffer, insn: &BpfInsn) {
        if insn.mode() != BPF_MEM {
            return;
        }
        let off = insn.off as i32;
        let src = bpf_to_aa(insn.src_reg());
        emit_addi(buf, AA_X16, AA_X29, off);
        match insn.size() {
            BPF_B => emit_strb(buf, src, AA_X16, 0),
            BPF_H => emit_strh(buf, src, AA_X16, 0),
            BPF_W => emit_strw(buf, src, AA_X16, 0),
            BPF_DW => emit_str(buf, src, AA_X16, 0),
            _ => {}
        }
    }

    fn emit_ldx(buf: &mut JitBuffer, insn: &BpfInsn) {
        if insn.mode() != BPF_MEM {
            return;
        }
        let off = insn.off as i32;
        let base = bpf_to_aa(insn.src_reg());
        let dst = bpf_to_aa(insn.dst_reg());
        emit_addi(buf, AA_X16, base, off);
        match insn.size() {
            BPF_B => emit_ldrb(buf, dst, AA_X16, 0),
            BPF_H => emit_ldrh(buf, dst, AA_X16, 0),
            BPF_W => emit_ldrw(buf, dst, AA_X16, 0),
            BPF_DW => emit_ldr(buf, dst, AA_X16, 0),
            _ => {}
        }
    }

    fn emit_ld_imm64(buf: &mut JitBuffer, insn: &BpfInsn, next_imm: i32) {
        let dst = bpf_to_aa(insn.dst_reg());
        let imm_lo = insn.imm as u64;
        let imm_hi = next_imm as u64;
        let val = (imm_hi << 32) | (imm_lo & 0xffffffff);
        emit_load_imm64(buf, dst, val);
    }

    fn emit_call(buf: &mut JitBuffer, helper_fn: HelperFn) {
        emit_load_imm64(buf, AA_X16, helper_fn as u64);
        emit_mov(buf, AA_X17, AA_X5);
        emit_mov(buf, AA_X5, AA_X4);
        emit_mov(buf, AA_X4, AA_X3);
        emit_mov(buf, AA_X3, AA_X2);
        emit_mov(buf, AA_X2, AA_X1);
        emit_mov(buf, AA_X1, AA_X0);
        emit_blr(buf, AA_X16);
    }

    fn insn_size(insn: &BpfInsn) -> usize {
        let class = insn.class();
        let use_imm = (insn.code & BPF_X) == 0;

        match class {
            BPF_ALU | BPF_ALU64 => {
                let alu_op = insn.alu_op();
                let imm_size = if use_imm { 16 } else { 4 };
                match alu_op {
                    BPF_DIV => imm_size + 12,
                    BPF_MOD => imm_size + 16,
                    _ => imm_size,
                }
            }
            BPF_JMP | BPF_JMP32 => {
                let op = insn.code & 0xf0;
                if op == BPF_EXIT {
                    20
                } else if op == 0x80 {
                    8 + 16 + 4
                } else if insn.code == (BPF_JMP | BPF_JA) || insn.code == (BPF_JMP32 | BPF_JA) {
                    4
                } else {
                    let cmp_size = if use_imm { 16 } else { 4 };
                    let extra = if op == BPF_JSET { 4 } else { 0 };
                    cmp_size + 4 + extra
                }
            }
            BPF_ST => {
                if insn.size() == BPF_DW {
                    28
                } else {
                    24
                }
            }
            BPF_STX => 8,
            BPF_LDX => 8,
            BPF_LD => {
                if insn.is_ld_dw_imm() {
                    16
                } else {
                    4
                }
            }
            _ => 4,
        }
    }
}

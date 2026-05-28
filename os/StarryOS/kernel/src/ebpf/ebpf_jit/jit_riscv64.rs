use super::{
    BpfInsn, HelperFn, JitBackend, JitBuffer,
    bpf_insn::{
        BPF_ADD, BPF_ALU, BPF_ALU64, BPF_AND, BPF_ARSH, BPF_B, BPF_DIV, BPF_DW, BPF_END, BPF_EXIT,
        BPF_H, BPF_JA, BPF_JEQ, BPF_JGE, BPF_JGT, BPF_JLE, BPF_JLT, BPF_JMP, BPF_JMP32, BPF_JNE,
        BPF_JSET, BPF_JSGE, BPF_JSGT, BPF_JSLE, BPF_JSLT, BPF_LD, BPF_LDX, BPF_LSH, BPF_MEM,
        BPF_MOD, BPF_MOV, BPF_MUL, BPF_NEG, BPF_OR, BPF_RSH, BPF_ST, BPF_STX, BPF_SUB, BPF_W,
        BPF_X, BPF_XOR,
    },
};

const RV_ZERO: u32 = 0;
const RV_RA: u32 = 1;
const RV_SP: u32 = 2;
const RV_T1: u32 = 6;
const RV_T2: u32 = 7;
const RV_S1: u32 = 9;
const RV_A0: u32 = 10;
const RV_A1: u32 = 11;
const RV_A2: u32 = 12;
const RV_A3: u32 = 13;
const RV_A4: u32 = 14;
const RV_A5: u32 = 15;
const RV_S2: u32 = 18;
const RV_S3: u32 = 19;
const RV_S4: u32 = 20;
const RV_S5: u32 = 21;
const RV_T6: u32 = 31;

const BPF_STACK_SIZE: usize = 512;
const CALLEE_SAVED_SIZE: usize = 48;
const FRAME_SIZE: usize = BPF_STACK_SIZE + CALLEE_SAVED_SIZE;

fn bpf_to_rv(r: u8) -> u32 {
    match r {
        0 => RV_A0,
        1 => RV_A1,
        2 => RV_A2,
        3 => RV_A3,
        4 => RV_A4,
        5 => RV_A5,
        6 => RV_S1,
        7 => RV_S2,
        8 => RV_S3,
        9 => RV_S4,
        10 => RV_S5,
        _ => RV_ZERO,
    }
}

fn rv_r(funct7: u32, rs2: u32, rs1: u32, funct3: u32, rd: u32) -> u32 {
    (funct7 << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | 0x33
}

fn rv_rw(funct7: u32, rs2: u32, rs1: u32, funct3: u32, rd: u32) -> u32 {
    rv_r(funct7, rs2, rs1, funct3, rd) | (0x3b ^ 0x33)
}

fn rv_i(imm: u32, rs1: u32, funct3: u32, rd: u32, opcode: u32) -> u32 {
    (imm << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
}

fn rv_s(imm: u32, rs2: u32, rs1: u32, funct3: u32) -> u32 {
    ((imm >> 5) << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | ((imm & 0x1f) << 7) | 0x23
}

fn rv_b(imm: u32, rs2: u32, rs1: u32, funct3: u32) -> u32 {
    let bit12 = (imm >> 12) & 1;
    let bits10_5 = (imm >> 5) & 0x3f;
    let bits4_1 = (imm >> 1) & 0xf;
    let bit11 = (imm >> 11) & 1;
    (bit12 << 31)
        | (bits10_5 << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (funct3 << 12)
        | (bits4_1 << 8)
        | (bit11 << 7)
        | 0x63
}

fn rv_u(imm: u32, rd: u32, opcode: u32) -> u32 {
    (imm << 12) | (rd << 7) | opcode
}

fn emit_add(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_r(0, rs2, rs1, 0, rd));
}

fn emit_addw(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_rw(0, rs2, rs1, 0, rd));
}

fn emit_sub(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_r(0x20, rs2, rs1, 0, rd));
}

fn emit_subw(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_rw(0x20, rs2, rs1, 0, rd));
}

fn emit_addi(buf: &mut JitBuffer, rd: u32, rs1: u32, imm: i32) {
    buf.emit_u32(rv_i(imm as u32, rs1, 0, rd, 0x13));
}

fn emit_addiw(buf: &mut JitBuffer, rd: u32, rs1: u32, imm: i32) {
    buf.emit_u32(rv_i(imm as u32, rs1, 0, rd, 0x1b));
}

fn emit_and(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_r(0, rs2, rs1, 7, rd));
}

fn emit_andw(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_rw(0, rs2, rs1, 7, rd));
}

fn emit_or(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_r(0, rs2, rs1, 6, rd));
}

fn emit_orw(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_rw(0, rs2, rs1, 6, rd));
}

fn emit_xor(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_r(0, rs2, rs1, 4, rd));
}

fn emit_xorw(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_rw(0, rs2, rs1, 4, rd));
}

fn emit_sll(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_r(0, rs2, rs1, 1, rd));
}

fn emit_sllw(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_rw(0, rs2, rs1, 1, rd));
}

fn emit_srl(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_r(0, rs2, rs1, 5, rd));
}

fn emit_srlw(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_rw(0, rs2, rs1, 5, rd));
}

fn emit_sra(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_r(0x20, rs2, rs1, 5, rd));
}

fn emit_sraw(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_rw(0x20, rs2, rs1, 5, rd));
}

fn emit_mul(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_r(1, rs2, rs1, 0, rd));
}

fn emit_mulw(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_rw(1, rs2, rs1, 0, rd));
}

fn emit_divu(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_r(1, rs2, rs1, 5, rd));
}

fn emit_divuw(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_rw(1, rs2, rs1, 5, rd));
}

fn emit_remu(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_r(1, rs2, rs1, 7, rd));
}

fn emit_remuw(buf: &mut JitBuffer, rd: u32, rs1: u32, rs2: u32) {
    buf.emit_u32(rv_rw(1, rs2, rs1, 7, rd));
}

fn emit_andi(buf: &mut JitBuffer, rd: u32, rs1: u32, imm: i32) {
    buf.emit_u32(rv_i(imm as u32, rs1, 7, rd, 0x13));
}

fn emit_ori(buf: &mut JitBuffer, rd: u32, rs1: u32, imm: i32) {
    buf.emit_u32(rv_i(imm as u32, rs1, 6, rd, 0x13));
}

fn emit_xori(buf: &mut JitBuffer, rd: u32, rs1: u32, imm: i32) {
    buf.emit_u32(rv_i(imm as u32, rs1, 4, rd, 0x13));
}

fn emit_zext32(buf: &mut JitBuffer, rd: u32) {
    emit_slli(buf, rd, rd, 32);
    emit_srli(buf, rd, rd, 32);
}

fn emit_slli(buf: &mut JitBuffer, rd: u32, rs1: u32, shamt: u32) {
    buf.emit_u32(rv_i(shamt, rs1, 1, rd, 0x13));
}

fn emit_srli(buf: &mut JitBuffer, rd: u32, rs1: u32, shamt: u32) {
    buf.emit_u32(rv_i(shamt, rs1, 5, rd, 0x13));
}

fn emit_srai(buf: &mut JitBuffer, rd: u32, rs1: u32, shamt: u32) {
    buf.emit_u32(rv_i(0x400 | shamt, rs1, 5, rd, 0x13));
}

fn emit_slliw(buf: &mut JitBuffer, rd: u32, rs1: u32, shamt: u32) {
    buf.emit_u32(rv_i(shamt, rs1, 1, rd, 0x1b));
}

fn emit_srliw(buf: &mut JitBuffer, rd: u32, rs1: u32, shamt: u32) {
    buf.emit_u32(rv_i(shamt, rs1, 5, rd, 0x1b));
}

fn emit_sraiw(buf: &mut JitBuffer, rd: u32, rs1: u32, shamt: u32) {
    buf.emit_u32(rv_i(0x400 | shamt, rs1, 5, rd, 0x1b));
}

fn emit_lui(buf: &mut JitBuffer, rd: u32, imm: u32) {
    buf.emit_u32(rv_u(imm, rd, 0x37));
}

fn emit_ld(buf: &mut JitBuffer, rd: u32, rs1: u32, off: i32) {
    buf.emit_u32(rv_i(off as u32, rs1, 3, rd, 0x03));
}

fn emit_lwu(buf: &mut JitBuffer, rd: u32, rs1: u32, off: i32) {
    buf.emit_u32(rv_i(off as u32, rs1, 6, rd, 0x03));
}

fn emit_lw(buf: &mut JitBuffer, rd: u32, rs1: u32, off: i32) {
    buf.emit_u32(rv_i(off as u32, rs1, 2, rd, 0x03));
}

fn emit_lhu(buf: &mut JitBuffer, rd: u32, rs1: u32, off: i32) {
    buf.emit_u32(rv_i(off as u32, rs1, 5, rd, 0x03));
}

fn emit_lh(buf: &mut JitBuffer, rd: u32, rs1: u32, off: i32) {
    buf.emit_u32(rv_i(off as u32, rs1, 1, rd, 0x03));
}

fn emit_lbu(buf: &mut JitBuffer, rd: u32, rs1: u32, off: i32) {
    buf.emit_u32(rv_i(off as u32, rs1, 4, rd, 0x03));
}

fn emit_lb(buf: &mut JitBuffer, rd: u32, rs1: u32, off: i32) {
    buf.emit_u32(rv_i(off as u32, rs1, 0, rd, 0x03));
}

fn emit_sd(buf: &mut JitBuffer, rs2: u32, rs1: u32, off: i32) {
    buf.emit_u32(rv_s(off as u32, rs2, rs1, 3));
}

fn emit_sw(buf: &mut JitBuffer, rs2: u32, rs1: u32, off: i32) {
    buf.emit_u32(rv_s(off as u32, rs2, rs1, 2));
}

fn emit_sh(buf: &mut JitBuffer, rs2: u32, rs1: u32, off: i32) {
    buf.emit_u32(rv_s(off as u32, rs2, rs1, 1));
}

fn emit_sb(buf: &mut JitBuffer, rs2: u32, rs1: u32, off: i32) {
    buf.emit_u32(rv_s(off as u32, rs2, rs1, 0));
}

fn emit_beq(buf: &mut JitBuffer, rs1: u32, rs2: u32, off: i32) {
    buf.emit_u32(rv_b(off as u32, rs2, rs1, 0));
}

fn emit_bne(buf: &mut JitBuffer, rs1: u32, rs2: u32, off: i32) {
    buf.emit_u32(rv_b(off as u32, rs2, rs1, 1));
}

fn emit_blt(buf: &mut JitBuffer, rs1: u32, rs2: u32, off: i32) {
    buf.emit_u32(rv_b(off as u32, rs2, rs1, 4));
}

fn emit_bge(buf: &mut JitBuffer, rs1: u32, rs2: u32, off: i32) {
    buf.emit_u32(rv_b(off as u32, rs2, rs1, 5));
}

fn emit_bltu(buf: &mut JitBuffer, rs1: u32, rs2: u32, off: i32) {
    buf.emit_u32(rv_b(off as u32, rs2, rs1, 6));
}

fn emit_bgeu(buf: &mut JitBuffer, rs1: u32, rs2: u32, off: i32) {
    buf.emit_u32(rv_b(off as u32, rs2, rs1, 7));
}

fn emit_jalr(buf: &mut JitBuffer, rd: u32, rs1: u32, off: i32) {
    buf.emit_u32(rv_i(off as u32, rs1, 0, rd, 0x67));
}

fn emit_auipc(buf: &mut JitBuffer, rd: u32, imm: u32) {
    buf.emit_u32(rv_u(imm, rd, 0x17));
}

fn emit_jal(buf: &mut JitBuffer, rd: u32, imm: i32) {
    let imm = imm as u32;
    let bit20 = (imm >> 20) & 1;
    let bits10_1 = (imm >> 1) & 0x3ff;
    let bit11 = (imm >> 11) & 1;
    let bits19_12 = (imm >> 12) & 0xff;
    buf.emit_u32(
        (bit20 << 31) | (bits10_1 << 21) | (bit11 << 20) | (bits19_12 << 12) | (rd << 7) | 0x6f,
    );
}

fn emit_ret(buf: &mut JitBuffer) {
    emit_jalr(buf, RV_ZERO, RV_RA, 0);
}

fn emit_mv(buf: &mut JitBuffer, rd: u32, rs: u32) {
    emit_addi(buf, rd, rs, 0);
}

fn emit_nop(buf: &mut JitBuffer) {
    buf.emit_u32(0x00000013);
}

fn emit_load_imm64_padded(buf: &mut JitBuffer, rd: u32, val: u64) {
    let start = buf.offset();
    emit_load_imm64(buf, rd, val);
    let emitted = buf.offset() - start;
    for _ in 0..((24 - emitted) / 4) {
        emit_nop(buf);
    }
}

fn emit_load_imm64(buf: &mut JitBuffer, rd: u32, val: u64) {
    let val_i = val as i64;
    if val_i >= -2048 && val_i < 2048 {
        emit_addi(buf, rd, RV_ZERO, val_i as i32);
        return;
    }
    if (val as u32 as i32) as i64 == val_i {
        let lo32 = val as u32;
        let lo12 = (lo32 << 20) >> 20;
        let hi20 = (lo32.wrapping_sub(lo12).wrapping_add(0x800)) >> 12;
        emit_lui(buf, rd, hi20 & 0xFFFFF);
        if lo12 != 0 {
            emit_addiw(buf, rd, rd, lo12 as i32);
        }
        return;
    }
    let upper = (val >> 32) as u32;
    let upper_lo12 = (upper << 20) >> 20;
    let upper_hi20 = (upper.wrapping_sub(upper_lo12).wrapping_add(0x800)) >> 12;
    emit_lui(buf, rd, upper_hi20 & 0xFFFFF);
    emit_addiw(buf, rd, rd, upper_lo12 as i32);
    emit_slli(buf, rd, rd, 32);
    let lower = val as u32;
    let lower_lo12 = (lower << 20) >> 20;
    let lower_hi20 = (lower.wrapping_sub(lower_lo12).wrapping_add(0x800)) >> 12;
    emit_lui(buf, RV_T1, lower_hi20 & 0xFFFFF);
    if lower_lo12 != 0 {
        emit_addiw(buf, RV_T1, RV_T1, lower_lo12 as i32);
    }
    emit_add(buf, rd, rd, RV_T1);
}

fn emit_load_imm32(buf: &mut JitBuffer, rd: u32, val: i32) {
    let needs_upper = (val as i32) < -2048 || (val as i32) >= 2048;
    if !needs_upper {
        emit_addi(buf, rd, RV_ZERO, val);
    } else {
        let val_u = val as u32;
        let lo12 = (val_u << 20) >> 20;
        let hi20 = (val_u.wrapping_sub(lo12).wrapping_add(0x800)) >> 12;
        emit_lui(buf, rd, hi20 & 0xFFFFF);
        if lo12 != 0 {
            emit_addiw(buf, rd, rd, lo12 as i32);
        }
    }
}

fn emit_add_offset(buf: &mut JitBuffer, rd: u32, rs: u32, off: i32) {
    if off >= -2048 && off < 2048 {
        emit_addi(buf, rd, rs, off);
    } else {
        emit_load_imm32(buf, RV_T1, off);
        emit_add(buf, rd, rs, RV_T1);
    }
}

pub(crate) struct Riscv64Backend;

impl JitBackend for Riscv64Backend {
    fn emit_prologue(buf: &mut JitBuffer) -> usize {
        emit_addi(buf, RV_SP, RV_SP, -(FRAME_SIZE as i32));
        emit_sd(buf, RV_RA, RV_SP, 0);
        emit_sd(buf, RV_S1, RV_SP, 8);
        emit_sd(buf, RV_S2, RV_SP, 16);
        emit_sd(buf, RV_S3, RV_SP, 24);
        emit_sd(buf, RV_S4, RV_SP, 32);
        emit_sd(buf, RV_S5, RV_SP, 40);
        emit_addi(buf, RV_S5, RV_SP, FRAME_SIZE as i32);
        emit_mv(buf, RV_A1, RV_A0);
        buf.offset()
    }

    fn emit_epilogue(buf: &mut JitBuffer) {
        emit_ld(buf, RV_RA, RV_SP, 0);
        emit_ld(buf, RV_S1, RV_SP, 8);
        emit_ld(buf, RV_S2, RV_SP, 16);
        emit_ld(buf, RV_S3, RV_SP, 24);
        emit_ld(buf, RV_S4, RV_SP, 32);
        emit_ld(buf, RV_S5, RV_SP, 40);
        emit_addi(buf, RV_SP, RV_SP, FRAME_SIZE as i32);
        emit_ret(buf);
    }

    fn emit_alu(buf: &mut JitBuffer, insn: &BpfInsn, is_64: bool) {
        let dst = bpf_to_rv(insn.dst_reg());
        let use_imm = (insn.code & BPF_X) == 0;
        let src = if use_imm {
            RV_T1
        } else {
            bpf_to_rv(insn.src_reg())
        };
        if use_imm {
            emit_load_imm64(buf, RV_T1, insn.imm as u64);
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
                let skip = buf.offset();
                if is_64 {
                    emit_beq(buf, src, RV_ZERO, 0);
                    emit_divu(buf, dst, dst, src);
                } else {
                    emit_beq(buf, src, RV_ZERO, 0);
                    emit_divuw(buf, dst, dst, src);
                }
                emit_jal(buf, RV_ZERO, 8);
                emit_addi(buf, dst, RV_ZERO, 0);
                unsafe {
                    let beq_ptr = buf.entry().add(skip) as *mut u32;
                    let beq_off = 12u32;
                    *beq_ptr = rv_b(beq_off, RV_ZERO, src, 0);
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
                    let shamt = if is_64 {
                        (insn.imm as u32) & 63
                    } else {
                        (insn.imm as u32) & 31
                    };
                    if is_64 {
                        emit_slli(buf, dst, dst, shamt);
                    } else {
                        emit_slliw(buf, dst, dst, shamt);
                    }
                } else if is_64 {
                    emit_andi(buf, RV_T2, src, 63);
                    emit_sll(buf, dst, dst, RV_T2);
                } else {
                    emit_andi(buf, RV_T2, src, 31);
                    emit_sllw(buf, dst, dst, RV_T2);
                }
            }
            BPF_RSH => {
                if use_imm {
                    let shamt = if is_64 {
                        (insn.imm as u32) & 63
                    } else {
                        (insn.imm as u32) & 31
                    };
                    if is_64 {
                        emit_srli(buf, dst, dst, shamt);
                    } else {
                        emit_srliw(buf, dst, dst, shamt);
                    }
                } else if is_64 {
                    emit_andi(buf, RV_T2, src, 63);
                    emit_srl(buf, dst, dst, RV_T2);
                } else {
                    emit_andi(buf, RV_T2, src, 31);
                    emit_srlw(buf, dst, dst, RV_T2);
                }
            }
            BPF_NEG => {
                if is_64 {
                    emit_sub(buf, dst, RV_ZERO, dst);
                } else {
                    emit_subw(buf, dst, RV_ZERO, dst);
                }
            }
            BPF_MOD => {
                let skip = buf.offset();
                if is_64 {
                    emit_beq(buf, src, RV_ZERO, 0);
                    emit_remu(buf, dst, dst, src);
                } else {
                    emit_beq(buf, src, RV_ZERO, 0);
                    emit_remuw(buf, dst, dst, src);
                }
                let end = buf.offset();
                unsafe {
                    let beq_ptr = buf.entry().add(skip) as *mut u32;
                    let beq_off = (end - skip) as u32;
                    *beq_ptr = rv_b(beq_off, RV_ZERO, src, 0);
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
                if is_64 {
                    emit_mv(buf, dst, src);
                } else {
                    emit_addiw(buf, dst, src, 0);
                }
            }
            BPF_ARSH => {
                if use_imm {
                    let shamt = if is_64 {
                        (insn.imm as u32) & 63
                    } else {
                        (insn.imm as u32) & 31
                    };
                    if is_64 {
                        emit_srai(buf, dst, dst, shamt);
                    } else {
                        emit_sraiw(buf, dst, dst, shamt);
                    }
                } else if is_64 {
                    emit_andi(buf, RV_T2, src, 63);
                    emit_sra(buf, dst, dst, RV_T2);
                } else {
                    emit_andi(buf, RV_T2, src, 31);
                    emit_sraw(buf, dst, dst, RV_T2);
                }
            }
            BPF_END => {
                log::warn!("eBPF JIT riscv64: BPF_END (byte-order conversion) not implemented");
            }
            _ => {}
        }

        if !is_64 && insn.alu_op() != BPF_ARSH {
            emit_zext32(buf, dst);
        }
    }

    fn emit_jmp(buf: &mut JitBuffer, insn: &BpfInsn, offsets: &[usize], pc: usize, is_64: bool) {
        let op = insn.code & 0xf0;

        if insn.code == (BPF_JMP | BPF_JA) || insn.code == (BPF_JMP32 | BPF_JA) {
            let target_pc = (pc as isize + 1 + insn.off as isize) as usize;
            if target_pc < offsets.len() {
                let off = offsets[target_pc] as isize - buf.offset() as isize;
                emit_auipc(buf, RV_T6, 0);
                emit_load_imm64_padded(buf, RV_T1, off as u64);
                emit_add(buf, RV_T6, RV_T6, RV_T1);
                emit_jalr(buf, RV_ZERO, RV_T6, 0);
            }
            return;
        }

        // BPF_CALL (0x80) is handled separately in compile()
        if op == 0x80 {
            return;
        }

        let dst = bpf_to_rv(insn.dst_reg());
        let use_imm = (insn.code & BPF_X) == 0;
        let src = if use_imm {
            RV_T1
        } else {
            bpf_to_rv(insn.src_reg())
        };

        if use_imm {
            if is_64 {
                emit_load_imm64(buf, RV_T1, insn.imm as u64);
            } else {
                emit_load_imm32(buf, RV_T1, insn.imm);
            }
        }

        if !is_64 {
            emit_zext32(buf, dst);
            emit_zext32(buf, src);
        }

        let target_pc = (pc as isize + 1 + insn.off as isize) as usize;

        match op {
            BPF_JEQ => {
                let start = buf.offset();
                emit_bne(buf, dst, src, 0);
                let auipc_pos = buf.offset();
                emit_auipc(buf, RV_T6, 0);
                let branch_off = (offsets[target_pc] as isize - auipc_pos as isize) as i32;
                emit_load_imm64_padded(buf, RV_T1, branch_off as u64);
                emit_add(buf, RV_T6, RV_T6, RV_T1);
                emit_jalr(buf, RV_ZERO, RV_T6, 0);
                let end = buf.offset();
                unsafe {
                    let ptr = buf.entry().add(start) as *mut u32;
                    *ptr = rv_b((end - start) as u32, src, dst, 1);
                }
            }
            BPF_JGT => {
                let start = buf.offset();
                emit_bgeu(buf, src, dst, 0);
                let auipc_pos = buf.offset();
                emit_auipc(buf, RV_T6, 0);
                let branch_off = (offsets[target_pc] as isize - auipc_pos as isize) as i32;
                emit_load_imm64_padded(buf, RV_T1, branch_off as u64);
                emit_add(buf, RV_T6, RV_T6, RV_T1);
                emit_jalr(buf, RV_ZERO, RV_T6, 0);
                let end = buf.offset();
                unsafe {
                    let ptr = buf.entry().add(start) as *mut u32;
                    *ptr = rv_b((end - start) as u32, dst, src, 7);
                }
            }
            BPF_JGE => {
                let start = buf.offset();
                emit_bltu(buf, dst, src, 0);
                let auipc_pos = buf.offset();
                emit_auipc(buf, RV_T6, 0);
                let branch_off = (offsets[target_pc] as isize - auipc_pos as isize) as i32;
                emit_load_imm64_padded(buf, RV_T1, branch_off as u64);
                emit_add(buf, RV_T6, RV_T6, RV_T1);
                emit_jalr(buf, RV_ZERO, RV_T6, 0);
                let end = buf.offset();
                unsafe {
                    let ptr = buf.entry().add(start) as *mut u32;
                    *ptr = rv_b((end - start) as u32, src, dst, 6);
                }
            }
            BPF_JSET => {
                emit_and(buf, RV_T2, dst, src);
                let start = buf.offset();
                emit_beq(buf, RV_T2, RV_ZERO, 0);
                let auipc_pos = buf.offset();
                emit_auipc(buf, RV_T6, 0);
                let branch_off = (offsets[target_pc] as isize - auipc_pos as isize) as i32;
                emit_load_imm64_padded(buf, RV_T1, branch_off as u64);
                emit_add(buf, RV_T6, RV_T6, RV_T1);
                emit_jalr(buf, RV_ZERO, RV_T6, 0);
                let end = buf.offset();
                unsafe {
                    let ptr = buf.entry().add(start) as *mut u32;
                    *ptr = rv_b((end - start) as u32, RV_ZERO, RV_T2, 0);
                }
            }
            BPF_JNE => {
                let start = buf.offset();
                emit_beq(buf, dst, src, 0);
                let auipc_pos = buf.offset();
                emit_auipc(buf, RV_T6, 0);
                let branch_off = (offsets[target_pc] as isize - auipc_pos as isize) as i32;
                emit_load_imm64_padded(buf, RV_T1, branch_off as u64);
                emit_add(buf, RV_T6, RV_T6, RV_T1);
                emit_jalr(buf, RV_ZERO, RV_T6, 0);
                let end = buf.offset();
                unsafe {
                    let ptr = buf.entry().add(start) as *mut u32;
                    *ptr = rv_b((end - start) as u32, src, dst, 0);
                }
            }
            BPF_JSGT => {
                let start = buf.offset();
                emit_bge(buf, src, dst, 0);
                let auipc_pos = buf.offset();
                emit_auipc(buf, RV_T6, 0);
                let branch_off = (offsets[target_pc] as isize - auipc_pos as isize) as i32;
                emit_load_imm64_padded(buf, RV_T1, branch_off as u64);
                emit_add(buf, RV_T6, RV_T6, RV_T1);
                emit_jalr(buf, RV_ZERO, RV_T6, 0);
                let end = buf.offset();
                unsafe {
                    let ptr = buf.entry().add(start) as *mut u32;
                    *ptr = rv_b((end - start) as u32, dst, src, 5);
                }
            }
            BPF_JSGE => {
                let start = buf.offset();
                emit_blt(buf, dst, src, 0);
                let auipc_pos = buf.offset();
                emit_auipc(buf, RV_T6, 0);
                let branch_off = (offsets[target_pc] as isize - auipc_pos as isize) as i32;
                emit_load_imm64_padded(buf, RV_T1, branch_off as u64);
                emit_add(buf, RV_T6, RV_T6, RV_T1);
                emit_jalr(buf, RV_ZERO, RV_T6, 0);
                let end = buf.offset();
                unsafe {
                    let ptr = buf.entry().add(start) as *mut u32;
                    *ptr = rv_b((end - start) as u32, src, dst, 4);
                }
            }
            BPF_JLT => {
                let start = buf.offset();
                emit_bgeu(buf, dst, src, 0);
                let auipc_pos = buf.offset();
                emit_auipc(buf, RV_T6, 0);
                let branch_off = (offsets[target_pc] as isize - auipc_pos as isize) as i32;
                emit_load_imm64_padded(buf, RV_T1, branch_off as u64);
                emit_add(buf, RV_T6, RV_T6, RV_T1);
                emit_jalr(buf, RV_ZERO, RV_T6, 0);
                let end = buf.offset();
                unsafe {
                    let ptr = buf.entry().add(start) as *mut u32;
                    *ptr = rv_b((end - start) as u32, src, dst, 7);
                }
            }
            BPF_JLE => {
                let start = buf.offset();
                emit_bltu(buf, src, dst, 0);
                let auipc_pos = buf.offset();
                emit_auipc(buf, RV_T6, 0);
                let branch_off = (offsets[target_pc] as isize - auipc_pos as isize) as i32;
                emit_load_imm64_padded(buf, RV_T1, branch_off as u64);
                emit_add(buf, RV_T6, RV_T6, RV_T1);
                emit_jalr(buf, RV_ZERO, RV_T6, 0);
                let end = buf.offset();
                unsafe {
                    let ptr = buf.entry().add(start) as *mut u32;
                    *ptr = rv_b((end - start) as u32, dst, src, 6);
                }
            }
            BPF_JSLT => {
                let start = buf.offset();
                emit_bge(buf, dst, src, 0);
                let auipc_pos = buf.offset();
                emit_auipc(buf, RV_T6, 0);
                let branch_off = (offsets[target_pc] as isize - auipc_pos as isize) as i32;
                emit_load_imm64_padded(buf, RV_T1, branch_off as u64);
                emit_add(buf, RV_T6, RV_T6, RV_T1);
                emit_jalr(buf, RV_ZERO, RV_T6, 0);
                let end = buf.offset();
                unsafe {
                    let ptr = buf.entry().add(start) as *mut u32;
                    *ptr = rv_b((end - start) as u32, src, dst, 5);
                }
            }
            BPF_JSLE => {
                let start = buf.offset();
                emit_blt(buf, src, dst, 0);
                let auipc_pos = buf.offset();
                emit_auipc(buf, RV_T6, 0);
                let branch_off = (offsets[target_pc] as isize - auipc_pos as isize) as i32;
                emit_load_imm64_padded(buf, RV_T1, branch_off as u64);
                emit_add(buf, RV_T6, RV_T6, RV_T1);
                emit_jalr(buf, RV_ZERO, RV_T6, 0);
                let end = buf.offset();
                unsafe {
                    let ptr = buf.entry().add(start) as *mut u32;
                    *ptr = rv_b((end - start) as u32, dst, src, 4);
                }
            }
            _ => {}
        }
    }

    fn emit_st(buf: &mut JitBuffer, insn: &BpfInsn) {
        if insn.mode() != BPF_MEM {
            return;
        }
        let off = insn.off as i32;
        let base = bpf_to_rv(insn.dst_reg());
        let adjusted_off = if base == RV_S5 {
            off - CALLEE_SAVED_SIZE as i32
        } else {
            off
        };
        emit_add_offset(buf, RV_T1, base, adjusted_off);
        let val = insn.imm as u64;
        match insn.size() {
            BPF_B => {
                emit_load_imm32(buf, RV_T2, val as i32);
                emit_sb(buf, RV_T2, RV_T1, 0);
            }
            BPF_H => {
                emit_load_imm32(buf, RV_T2, val as i32);
                emit_sh(buf, RV_T2, RV_T1, 0);
            }
            BPF_W => {
                emit_load_imm32(buf, RV_T2, val as i32);
                emit_sw(buf, RV_T2, RV_T1, 0);
            }
            BPF_DW => {
                emit_load_imm64(buf, RV_T2, val);
                emit_sd(buf, RV_T2, RV_T1, 0);
            }
            _ => {}
        }
    }

    fn emit_stx(buf: &mut JitBuffer, insn: &BpfInsn) {
        if insn.mode() != BPF_MEM {
            return;
        }
        let off = insn.off as i32;
        let src = bpf_to_rv(insn.src_reg());
        let base = bpf_to_rv(insn.dst_reg());
        let adjusted_off = if base == RV_S5 {
            off - CALLEE_SAVED_SIZE as i32
        } else {
            off
        };
        emit_add_offset(buf, RV_T1, base, adjusted_off);
        match insn.size() {
            BPF_B => emit_sb(buf, src, RV_T1, 0),
            BPF_H => emit_sh(buf, src, RV_T1, 0),
            BPF_W => emit_sw(buf, src, RV_T1, 0),
            BPF_DW => emit_sd(buf, src, RV_T1, 0),
            _ => {}
        }
    }

    fn emit_ldx(buf: &mut JitBuffer, insn: &BpfInsn) {
        if insn.mode() != BPF_MEM {
            return;
        }
        let off = insn.off as i32;
        let src = bpf_to_rv(insn.src_reg());
        let dst = bpf_to_rv(insn.dst_reg());
        let adjusted_off = if src == RV_S5 {
            off - CALLEE_SAVED_SIZE as i32
        } else {
            off
        };
        emit_add_offset(buf, RV_T1, src, adjusted_off);
        match insn.size() {
            BPF_B => {
                emit_lbu(buf, dst, RV_T1, 0);
            }
            BPF_H => {
                emit_lhu(buf, dst, RV_T1, 0);
            }
            BPF_W => {
                emit_lwu(buf, dst, RV_T1, 0);
            }
            BPF_DW => {
                emit_ld(buf, dst, RV_T1, 0);
            }
            _ => {}
        }
    }

    fn emit_ld_imm64(buf: &mut JitBuffer, insn: &BpfInsn, next_imm: i32) {
        let dst = bpf_to_rv(insn.dst_reg());
        let imm_lo = insn.imm as u64;
        let imm_hi = next_imm as u64;
        let val = (imm_hi << 32) | (imm_lo & 0xffffffff);
        let start = buf.offset();
        emit_load_imm64(buf, dst, val);
        let emitted = buf.offset() - start;
        for _ in 0..((24 - emitted) / 4) {
            emit_nop(buf);
        }
    }

    fn emit_call(buf: &mut JitBuffer, helper_fn: HelperFn) {
        emit_mv(buf, RV_T2, RV_A5);
        emit_mv(buf, RV_A0, RV_A1);
        emit_mv(buf, RV_A1, RV_A2);
        emit_mv(buf, RV_A2, RV_A3);
        emit_mv(buf, RV_A3, RV_A4);
        emit_mv(buf, RV_A4, RV_T2);

        emit_load_imm64_padded(buf, RV_T1, helper_fn as u64);
        emit_jalr(buf, RV_RA, RV_T1, 0);
    }

    fn insn_size(insn: &BpfInsn) -> usize {
        let class = insn.class();
        let use_imm = (insn.code & BPF_X) == 0;

        match class {
            BPF_ALU | BPF_ALU64 => {
                let alu_op = insn.alu_op();
                let load_size = if use_imm {
                    if insn.imm >= -2048 && insn.imm < 2048 {
                        4
                    } else if (insn.imm as u32) & 0xFFF == 0 {
                        4
                    } else {
                        8
                    }
                } else {
                    0
                };
                let zext_size = if class == BPF_ALU && alu_op != BPF_ARSH {
                    8
                } else {
                    0
                };
                let op_size = match alu_op {
                    BPF_DIV => 16,
                    BPF_MOD => 8,
                    BPF_LSH | BPF_RSH | BPF_ARSH => {
                        if use_imm {
                            4
                        } else {
                            8
                        }
                    }
                    _ => 4,
                };
                load_size + zext_size + op_size
            }
            BPF_JMP | BPF_JMP32 => {
                let op = insn.code & 0xf0;
                if op == BPF_EXIT {
                    32
                } else if op == 0x80 {
                    52
                } else if insn.code == (BPF_JMP | BPF_JA) || insn.code == (BPF_JMP32 | BPF_JA) {
                    36
                } else {
                    let imm_size = if use_imm {
                        if insn.imm >= -2048 && insn.imm < 2048 {
                            4
                        } else if (insn.imm as u32) & 0xFFF == 0 {
                            4
                        } else {
                            8
                        }
                    } else {
                        0
                    };
                    let zext_size = if class == BPF_JMP32 { 16 } else { 0 };
                    let jset_extra = if op == BPF_JSET { 4 } else { 0 };
                    imm_size + zext_size + 40 + jset_extra
                }
            }
            BPF_ST | BPF_STX => {
                let base = bpf_to_rv(insn.dst_reg());
                let effective_off = if base == RV_S5 {
                    (insn.off as i32) - CALLEE_SAVED_SIZE as i32
                } else {
                    insn.off as i32
                };
                let off_size = if effective_off >= -2048 && effective_off < 2048 {
                    0
                } else {
                    8
                };
                let imm_size = if class == BPF_ST {
                    if insn.imm >= -2048 && insn.imm < 2048 {
                        4
                    } else if (insn.imm as u32) & 0xFFF == 0 {
                        4
                    } else {
                        8
                    }
                } else {
                    0
                };
                4 + off_size + imm_size + 4
            }
            BPF_LDX => {
                let base = bpf_to_rv(insn.src_reg());
                let effective_off = if base == RV_S5 {
                    (insn.off as i32) - CALLEE_SAVED_SIZE as i32
                } else {
                    insn.off as i32
                };
                let off_size = if effective_off >= -2048 && effective_off < 2048 {
                    0
                } else {
                    8
                };
                4 + off_size + 4
            }
            BPF_LD => {
                if insn.is_ld_dw_imm() {
                    24
                } else {
                    4
                }
            }
            _ => 4,
        }
    }
}

#[cfg(test)]
mod tests_arch_independent {
    fn compute_hi20_lo12(val: u32) -> (u32, u32) {
        let lo12 = (val << 20) >> 20;
        let hi20 = (val.wrapping_sub(lo12).wrapping_add(0x800)) >> 12;
        (hi20 & 0xFFFFF, lo12)
    }

    fn reconstruct_from_hi20_lo12(hi20: u32, lo12: u32) -> u64 {
        let lui_val = ((hi20 as i32).wrapping_shl(12)) as i64;
        (lui_val.wrapping_add(lo12 as i32 as i64)) as u64
    }

    fn reconstruct_64bit(
        upper_hi20: u32,
        upper_lo12: u32,
        lower_hi20: u32,
        lower_lo12: u32,
    ) -> u64 {
        let upper = reconstruct_from_hi20_lo12(upper_hi20, upper_lo12);
        let lower = reconstruct_from_hi20_lo12(lower_hi20, lower_lo12);
        ((upper as u64) << 32).wrapping_add(lower)
    }

    #[test]
    fn test_hi20_lo12_identity() {
        for val in [
            0x00000000u32,
            0x00000001,
            0x00000FFF,
            0xFFFFF000,
            0xFFFFFFFF,
        ] {
            let (hi20, lo12) = compute_hi20_lo12(val);
            let reconstructed = reconstruct_from_hi20_lo12(hi20, lo12);
            assert_eq!(
                reconstructed, val as u64,
                "val={val:#010x}: hi20={hi20:#010x} lo12={lo12:#010x}"
            );
        }
    }

    #[test]
    fn test_hi20_lo12_bit31_set() {
        let val: u32 = 0x80000000;
        let (hi20, lo12) = compute_hi20_lo12(val);
        assert_eq!(hi20, 0x80000, "hi20 should be 0x80000");
        assert_eq!(lo12, 0, "lo12 should be 0");
        let reconstructed = reconstruct_from_hi20_lo12(hi20, lo12);
        assert_eq!(reconstructed, val as u64);
    }

    #[test]
    fn test_hi20_lo12_large_negative() {
        let val: u32 = 0xC0000000;
        let (hi20, lo12) = compute_hi20_lo12(val);
        assert_ne!(hi20, 0, "hi20 should not be 0");
        let reconstructed = reconstruct_from_hi20_lo12(hi20, lo12);
        assert_eq!(reconstructed, val as u64);
    }

    #[test]
    fn test_hi20_lo12_0x7FFFFFFF() {
        let val: u32 = 0x7FFFFFFF;
        let (hi20, lo12) = compute_hi20_lo12(val);
        assert_ne!(hi20, 0, "hi20 should not be 0");
        let reconstructed = reconstruct_from_hi20_lo12(hi20, lo12);
        assert_eq!(reconstructed, val as u64);
    }

    #[test]
    fn test_64bit_reconstruct_0x1_80000000() {
        let val: u64 = 0x0000_0001_8000_0000;
        let upper = (val >> 32) as u32;
        let lower = val as u32;
        let (upper_hi20, upper_lo12) = compute_hi20_lo12(upper);
        let (lower_hi20, lower_lo12) = compute_hi20_lo12(lower);
        assert_ne!(lower_hi20, 0, "lower_hi20 should not be 0 for 0x80000000");
        let reconstructed = reconstruct_64bit(upper_hi20, upper_lo12, lower_hi20, lower_lo12);
        assert_eq!(reconstructed, val, "val={val:#018x}");
    }

    #[test]
    fn test_64bit_reconstruct_all_ones() {
        let val: u64 = 0xFFFF_FFFF_FFFF_FFFF;
        let upper = (val >> 32) as u32;
        let lower = val as u32;
        let (upper_hi20, upper_lo12) = compute_hi20_lo12(upper);
        let (lower_hi20, lower_lo12) = compute_hi20_lo12(lower);
        let reconstructed = reconstruct_64bit(upper_hi20, upper_lo12, lower_hi20, lower_lo12);
        assert_eq!(reconstructed, val);
    }

    #[test]
    fn test_64bit_reconstruct_alternating() {
        let val: u64 = 0x5555_5555_AAAA_AAAA;
        let upper = (val >> 32) as u32;
        let lower = val as u32;
        let (upper_hi20, upper_lo12) = compute_hi20_lo12(upper);
        let (lower_hi20, lower_lo12) = compute_hi20_lo12(lower);
        let reconstructed = reconstruct_64bit(upper_hi20, upper_lo12, lower_hi20, lower_lo12);
        assert_eq!(reconstructed, val);
    }

    #[test]
    fn test_hi20_masked_to_20_bits() {
        for val in 0u32..4096 {
            let (hi20, _) = compute_hi20_lo12(val * 0x1000);
            assert_eq!(
                hi20 & !0xFFFFF,
                0,
                "hi20 must fit in 20 bits for val={val:#010x}"
            );
        }
    }
}

#[cfg(all(test, target_arch = "riscv64"))]
mod tests {
    use super::*;

    fn new_buf() -> JitBuffer {
        JitBuffer::new(4096).unwrap()
    }

    fn buf_as_u32_slice(buf: &JitBuffer) -> &[u32] {
        unsafe { core::slice::from_raw_parts(buf.entry() as *const u32, buf.offset() / 4) }
    }

    fn decode_lui(insn: u32) -> (u32, u32) {
        let rd = (insn >> 7) & 0x1f;
        let imm = (insn >> 12) & 0xfffff;
        (rd, imm)
    }

    fn decode_addiw(insn: u32) -> (u32, u32, i32) {
        let rd = (insn >> 7) & 0x1f;
        let rs1 = (insn >> 15) & 0x1f;
        let imm = ((insn as i32) << 20) >> 20;
        (rd, rs1, imm)
    }

    fn decode_slli(insn: u32) -> (u32, u32, u32) {
        let rd = (insn >> 7) & 0x1f;
        let rs1 = (insn >> 15) & 0x1f;
        let shamt = (insn >> 20) & 0x3f;
        (rd, rs1, shamt)
    }

    fn decode_add(insn: u32) -> (u32, u32, u32) {
        let rd = (insn >> 7) & 0x1f;
        let rs1 = (insn >> 15) & 0x1f;
        let rs2 = (insn >> 20) & 0x1f;
        (rd, rs1, rs2)
    }

    fn decode_addi(insn: u32) -> (u32, u32, i32) {
        decode_addiw(insn)
    }

    fn sign_extend_20(imm20: u32) -> i64 {
        ((imm20 << 12) as i32 as i64) << 12 >> 12
    }

    fn reconstruct_load_imm64(insns: &[u32]) -> u64 {
        let mut idx = 0;
        let first_opcode = insns[idx] & 0x7f;
        if first_opcode == 0x13 {
            let (_, _, imm) = decode_addi(insns[idx]);
            return imm as u64;
        }
        assert_eq!(first_opcode, 0x37, "expected LUI");
        let (rd1, imm20_hi) = decode_lui(insns[idx]);
        idx += 1;
        let upper_val;
        if idx < insns.len() && (insns[idx] & 0x7f) == 0x1b {
            let (rd2, rs1, lo12) = decode_addiw(insns[idx]);
            assert_eq!(rd2, rd1);
            assert_eq!(rs1, rd1);
            upper_val = ((imm20_hi as i32) << 12).wrapping_add(lo12);
            idx += 1;
        } else {
            upper_val = (imm20_hi as i32) << 12;
        }
        if idx >= insns.len() || (insns[idx] & 0x7f) != 0x13 {
            return upper_val as u64;
        }
        let (rd3, rs1_3, shamt) = decode_slli(insns[idx]);
        assert_eq!(rd3, rd1);
        assert_eq!(rs1_3, rd1);
        assert_eq!(shamt, 32);
        let shifted = (upper_val as u64) << 32;
        idx += 1;
        let (_, imm20_lo) = decode_lui(insns[idx]);
        idx += 1;
        let (rd_t1, rs1_t1, lo12_lo) = decode_addiw(insns[idx]);
        let lower_val = ((imm20_lo as i32) << 12).wrapping_add(lo12_lo);
        let lower_extended = lower_val as i64 as u64;
        shifted.wrapping_add(lower_extended)
    }

    #[test]
    fn test_load_imm64_small_positive() {
        let mut buf = new_buf();
        emit_load_imm64(&mut buf, RV_A0, 42);
        assert_eq!(buf.offset(), 4);
        let insns = buf_as_u32_slice(&buf);
        let (_, _, imm) = decode_addi(insns[0]);
        assert_eq!(imm, 42);
    }

    #[test]
    fn test_load_imm64_small_negative() {
        let mut buf = new_buf();
        emit_load_imm64(&mut buf, RV_A0, (-1i64) as u64);
        assert_eq!(buf.offset(), 4);
        let insns = buf_as_u32_slice(&buf);
        let (_, _, imm) = decode_addi(insns[0]);
        assert_eq!(imm, -1);
    }

    #[test]
    fn test_load_imm64_32bit_value() {
        let mut buf = new_buf();
        let val: u64 = 0x12345000;
        emit_load_imm64(&mut buf, RV_A0, val);
        let insns = buf_as_u32_slice(&buf);
        assert_eq!(reconstruct_load_imm64(insns), val);
    }

    #[test]
    fn test_load_imm64_bit31_set() {
        let mut buf = new_buf();
        let val: u64 = 0x0000_0001_8000_0000;
        emit_load_imm64(&mut buf, RV_A0, val);
        let insns = buf_as_u32_slice(&buf);
        assert_eq!(reconstruct_load_imm64(insns), val);
    }

    #[test]
    fn test_load_imm64_all_ones() {
        let mut buf = new_buf();
        let val: u64 = 0xFFFF_FFFF_FFFF_FFFF;
        emit_load_imm64(&mut buf, RV_A0, val);
        let insns = buf_as_u32_slice(&buf);
        assert_eq!(reconstruct_load_imm64(insns), val);
    }

    #[test]
    fn test_load_imm64_high_bit_only() {
        let mut buf = new_buf();
        let val: u64 = 0x8000_0000_0000_0000;
        emit_load_imm64(&mut buf, RV_A0, val);
        let insns = buf_as_u32_slice(&buf);
        assert_eq!(reconstruct_load_imm64(insns), val);
    }

    #[test]
    fn test_load_imm64_max_positive() {
        let mut buf = new_buf();
        let val: u64 = 0x7FFF_FFFF_FFFF_FFFF;
        emit_load_imm64(&mut buf, RV_A0, val);
        let insns = buf_as_u32_slice(&buf);
        assert_eq!(reconstruct_load_imm64(insns), val);
    }

    #[test]
    fn test_load_imm64_alternating_bits() {
        let mut buf = new_buf();
        let val: u64 = 0x5555_5555_AAAA_AAAA;
        emit_load_imm64(&mut buf, RV_A0, val);
        let insns = buf_as_u32_slice(&buf);
        assert_eq!(reconstruct_load_imm64(insns), val);
    }

    #[test]
    fn test_load_imm64_upper_ffff_lower_0() {
        let mut buf = new_buf();
        let val: u64 = 0xFFFF_FFFF_0000_0000;
        emit_load_imm64(&mut buf, RV_A0, val);
        let insns = buf_as_u32_slice(&buf);
        assert_eq!(reconstruct_load_imm64(insns), val);
    }

    #[test]
    fn test_load_imm64_zero_lower() {
        let mut buf = new_buf();
        let val: u64 = 0x1234_5678_0000_0000;
        emit_load_imm64(&mut buf, RV_A0, val);
        let insns = buf_as_u32_slice(&buf);
        assert_eq!(reconstruct_load_imm64(insns), val);
    }

    #[test]
    fn test_insn_size_alu_imm_small() {
        let mut insn = BpfInsn::default();
        insn.code = BPF_ALU64 | BPF_ADD;
        insn.imm = 100;
        let size = Riscv64Backend::insn_size(&insn);
        assert_eq!(size, 8);
    }

    #[test]
    fn test_insn_size_exit() {
        let mut insn = BpfInsn::default();
        insn.code = BPF_JMP | BPF_EXIT;
        let size = Riscv64Backend::insn_size(&insn);
        assert_eq!(size, 32);
    }

    #[test]
    fn test_insn_size_ld_dw_imm() {
        let mut insn = BpfInsn::default();
        insn.code = BPF_LD | BPF_DW;
        assert!(insn.is_ld_dw_imm());
        let size = Riscv64Backend::insn_size(&insn);
        assert_eq!(size, 24);
    }

    #[test]
    fn test_alu_add32_zext() {
        let mut buf = new_buf();
        let mut insn = BpfInsn::default();
        insn.code = BPF_ALU | BPF_ADD | BPF_X;
        insn.dst_reg = 1;
        insn.src_reg = 2;
        let expected_size = Riscv64Backend::insn_size(&insn);
        Riscv64Backend::emit_alu(&mut buf, &insn, false);
        assert_eq!(buf.offset(), expected_size);
        let insns = buf_as_u32_slice(&buf);
        let last = insns[insns.len() - 1];
        let prev = insns[insns.len() - 2];
        let (_, _, shamt1) = decode_slli(prev);
        let (_, _, shamt2) = decode_slli_with_func(last);
        assert_eq!(shamt1, 32);
        assert_eq!(shamt2, 32);
    }

    fn decode_slli_with_func(insn: u32) -> (u32, u32, u32) {
        let funct3 = (insn >> 12) & 0x7;
        let opcode = insn & 0x7f;
        assert_eq!(opcode, 0x13);
        assert!(funct3 == 1 || funct3 == 5);
        let rd = (insn >> 7) & 0x1f;
        let rs1 = (insn >> 15) & 0x1f;
        let shamt = (insn >> 20) & 0x3f;
        (rd, rs1, shamt)
    }

    #[test]
    fn test_alu_mov32_zext() {
        let mut buf = new_buf();
        let mut insn = BpfInsn::default();
        insn.code = BPF_ALU | BPF_MOV | BPF_X;
        insn.dst_reg = 1;
        insn.src_reg = 2;
        let expected_size = Riscv64Backend::insn_size(&insn);
        Riscv64Backend::emit_alu(&mut buf, &insn, false);
        assert_eq!(buf.offset(), expected_size);
    }

    #[test]
    fn test_div32_emit_size_matches_insn_size() {
        let mut buf = new_buf();
        let mut insn = BpfInsn::default();
        insn.code = BPF_ALU64 | BPF_DIV | BPF_X;
        insn.dst_reg = 1;
        insn.src_reg = 2;
        let expected_size = Riscv64Backend::insn_size(&insn);
        Riscv64Backend::emit_alu(&mut buf, &insn, true);
        assert_eq!(buf.offset(), expected_size);
    }

    #[test]
    fn test_mod64_emit_size_matches_insn_size() {
        let mut buf = new_buf();
        let mut insn = BpfInsn::default();
        insn.code = BPF_ALU64 | BPF_MOD | BPF_X;
        insn.dst_reg = 1;
        insn.src_reg = 2;
        let expected_size = Riscv64Backend::insn_size(&insn);
        Riscv64Backend::emit_alu(&mut buf, &insn, true);
        assert_eq!(buf.offset(), expected_size);
    }

    #[test]
    fn test_mod32_emit_size_matches_insn_size() {
        let mut buf = new_buf();
        let mut insn = BpfInsn::default();
        insn.code = BPF_ALU | BPF_MOD | BPF_X;
        insn.dst_reg = 1;
        insn.src_reg = 2;
        let expected_size = Riscv64Backend::insn_size(&insn);
        Riscv64Backend::emit_alu(&mut buf, &insn, false);
        assert_eq!(buf.offset(), expected_size);
    }

    #[test]
    fn test_alu_imm_add64_emit_size() {
        let mut buf = new_buf();
        let mut insn = BpfInsn::default();
        insn.code = BPF_ALU64 | BPF_ADD;
        insn.dst_reg = 1;
        insn.imm = 42;
        let expected_size = Riscv64Backend::insn_size(&insn);
        Riscv64Backend::emit_alu(&mut buf, &insn, true);
        assert_eq!(buf.offset(), expected_size);
    }

    #[test]
    fn test_alu_imm_add32_emit_size() {
        let mut buf = new_buf();
        let mut insn = BpfInsn::default();
        insn.code = BPF_ALU | BPF_ADD;
        insn.dst_reg = 1;
        insn.imm = 42;
        let expected_size = Riscv64Backend::insn_size(&insn);
        Riscv64Backend::emit_alu(&mut buf, &insn, false);
        assert_eq!(buf.offset(), expected_size);
    }

    #[test]
    fn test_arsh32_no_zext() {
        let mut buf = new_buf();
        let mut insn = BpfInsn::default();
        insn.code = BPF_ALU | BPF_ARSH;
        insn.dst_reg = 1;
        insn.imm = 1;
        let expected_size = Riscv64Backend::insn_size(&insn);
        Riscv64Backend::emit_alu(&mut buf, &insn, false);
        assert_eq!(buf.offset(), expected_size);
    }

    #[test]
    fn test_st_ldx_frame_pointer_offset() {
        let mut insn = BpfInsn::default();
        insn.code = BPF_MEM | BPF_STX | BPF_DW;
        insn.dst_reg = 10;
        insn.src_reg = 0;
        insn.off = -8;
        let expected_size = Riscv64Backend::insn_size(&insn);
        assert!(expected_size >= 8);
    }

    #[test]
    fn test_insn_size_alu32_all_ops() {
        for (op_code, _name) in [BPF_ADD, BPF_SUB, BPF_MUL, BPF_OR, BPF_AND, BPF_XOR]
            .iter()
            .zip(["add", "sub", "mul", "or", "and", "xor"].iter())
        {
            let mut insn = BpfInsn::default();
            insn.code = BPF_ALU | op_code | BPF_X;
            insn.dst_reg = 1;
            insn.src_reg = 2;
            let size = Riscv64Backend::insn_size(&insn);
            assert!(size >= 12, "ALU32 {} should include op(4) + zext(8)", _name);
        }
    }
}

use super::{
    super::bpf_insn::{
        BPF_ADD, BPF_AND, BPF_ARSH, BPF_B, BPF_DIV, BPF_DW, BPF_H, BPF_JA, BPF_JEQ, BPF_JGE,
        BPF_JGT, BPF_JLE, BPF_JLT, BPF_JNE, BPF_JSET, BPF_JSGE, BPF_JSGT, BPF_JSLE, BPF_JSLT,
        BPF_LSH, BPF_MEM, BPF_MOD, BPF_MOV, BPF_MUL, BPF_NEG, BPF_OR, BPF_RSH, BPF_SUB, BPF_W,
        BPF_X, BPF_XOR,
    },
    BPF_ALU, BPF_ALU64, BPF_EXIT, BPF_JMP, BPF_JMP32, BPF_LD, BPF_LDX, BPF_ST, BPF_STX, BpfInsn,
    HelperFn, JitBackend, JitBuffer,
};

const X86_RAX: u8 = 0;
const X86_RCX: u8 = 1;
const X86_RDX: u8 = 2;
const X86_RBX: u8 = 3;
const X86_RSP: u8 = 4;
const X86_RBP: u8 = 5;
const X86_RSI: u8 = 6;
const X86_RDI: u8 = 7;
const X86_R8: u8 = 8;
const _X86_R9: u8 = 9;
const X86_R10: u8 = 10;
const X86_R11: u8 = 11;
const _X86_R12: u8 = 12;
const X86_R13: u8 = 13;
const X86_R14: u8 = 14;
const X86_R15: u8 = 15;

fn bpf_to_x86(r: u8) -> u8 {
    match r {
        0 => X86_RAX,
        1 => X86_RDI,
        2 => X86_RSI,
        3 => X86_RDX,
        4 => X86_RCX,
        5 => X86_R8,
        6 => X86_RBX,
        7 => X86_R13,
        8 => X86_R14,
        9 => X86_R15,
        10 => X86_RBP,
        _ => X86_RAX,
    }
}

fn need_rex(r: u8) -> bool {
    r >= 8
}

fn emit_rex(buf: &mut JitBuffer, w: bool, r: u8, x: bool, b: u8) {
    let mut rex: u8 = 0x40;
    if w {
        rex |= 0x08;
    }
    if need_rex(r) {
        rex |= 0x04;
    }
    if x {
        rex |= 0x02;
    }
    if need_rex(b) {
        rex |= 0x01;
    }
    buf.emit_u8(rex);
}

fn emit_modrm(buf: &mut JitBuffer, mod_bits: u8, reg: u8, rm: u8) {
    buf.emit_u8((mod_bits << 6) | ((reg & 7) << 3) | (rm & 7));
}

fn emit_rex_if(buf: &mut JitBuffer, r: u8, b: u8) {
    if need_rex(r) || need_rex(b) {
        emit_rex(buf, false, r, false, b);
    }
}

fn emit_rex_w(buf: &mut JitBuffer, r: u8, b: u8) {
    emit_rex(buf, true, r, false, b);
}

fn emit_modrm_disp(buf: &mut JitBuffer, reg: u8, rm: u8, disp: i32) {
    if disp == 0 && (rm & 7) != X86_RBP {
        emit_modrm(buf, 0, reg, rm);
    } else if (-128..=127).contains(&disp) {
        emit_modrm(buf, 1, reg, rm);
        buf.emit_u8(disp as u8);
    } else {
        emit_modrm(buf, 2, reg, rm);
        buf.emit_u32(disp as u32);
    }
}

fn emit_push(buf: &mut JitBuffer, r: u8) {
    if need_rex(r) {
        buf.emit_u8(0x41);
    }
    buf.emit_u8(0x50 | (r & 7));
}

fn emit_pop(buf: &mut JitBuffer, r: u8) {
    if need_rex(r) {
        buf.emit_u8(0x41);
    }
    buf.emit_u8(0x58 | (r & 7));
}

fn emit_mov_reg64(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_w(buf, src, dst);
    buf.emit_u8(0x89);
    emit_modrm(buf, 3, src, dst);
}

fn emit_mov_reg32(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_if(buf, src, dst);
    buf.emit_u8(0x89);
    emit_modrm(buf, 3, src, dst);
}

fn emit_mov_imm64(buf: &mut JitBuffer, dst: u8, imm: u64) {
    emit_rex_w(buf, 0, dst);
    buf.emit_u8(0xB8 | (dst & 7));
    buf.emit_u32(imm as u32);
    buf.emit_u32((imm >> 32) as u32);
}

fn emit_mov_imm32(buf: &mut JitBuffer, dst: u8, imm: i32) {
    emit_rex_if(buf, 0, dst);
    buf.emit_u8(0xC7);
    emit_modrm(buf, 3, 0, dst);
    buf.emit_u32(imm as u32);
}

fn emit_add_reg64(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_w(buf, src, dst);
    buf.emit_u8(0x01);
    emit_modrm(buf, 3, src, dst);
}

fn emit_sub_reg64(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_w(buf, src, dst);
    buf.emit_u8(0x29);
    emit_modrm(buf, 3, src, dst);
}

fn emit_add_reg32(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_if(buf, src, dst);
    buf.emit_u8(0x01);
    emit_modrm(buf, 3, src, dst);
}

fn emit_sub_reg32(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_if(buf, src, dst);
    buf.emit_u8(0x29);
    emit_modrm(buf, 3, src, dst);
}

fn emit_imul_reg64(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_w(buf, dst, src);
    buf.emit_u8(0x0F);
    buf.emit_u8(0xAF);
    emit_modrm(buf, 3, dst, src);
}

fn emit_imul_reg32(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_if(buf, dst, src);
    buf.emit_u8(0x0F);
    buf.emit_u8(0xAF);
    emit_modrm(buf, 3, dst, src);
}

fn emit_xor_reg64(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_w(buf, src, dst);
    buf.emit_u8(0x31);
    emit_modrm(buf, 3, src, dst);
}

fn emit_xor_reg32(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_if(buf, src, dst);
    buf.emit_u8(0x31);
    emit_modrm(buf, 3, src, dst);
}

fn emit_or_reg64(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_w(buf, src, dst);
    buf.emit_u8(0x09);
    emit_modrm(buf, 3, src, dst);
}

fn emit_or_reg32(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_if(buf, src, dst);
    buf.emit_u8(0x09);
    emit_modrm(buf, 3, src, dst);
}

fn emit_and_reg64(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_w(buf, src, dst);
    buf.emit_u8(0x21);
    emit_modrm(buf, 3, src, dst);
}

fn emit_and_reg32(buf: &mut JitBuffer, dst: u8, src: u8) {
    emit_rex_if(buf, src, dst);
    buf.emit_u8(0x21);
    emit_modrm(buf, 3, src, dst);
}

fn emit_neg_reg64(buf: &mut JitBuffer, r: u8) {
    emit_rex_w(buf, 0, r);
    buf.emit_u8(0xF7);
    emit_modrm(buf, 3, 3, r);
}

fn emit_neg_reg32(buf: &mut JitBuffer, r: u8) {
    emit_rex_if(buf, 0, r);
    buf.emit_u8(0xF7);
    emit_modrm(buf, 3, 3, r);
}

fn emit_shl_reg64(buf: &mut JitBuffer, dst: u8) {
    emit_rex_w(buf, 0, dst);
    buf.emit_u8(0xD3);
    emit_modrm(buf, 3, 4, dst);
}

fn emit_shr_reg64(buf: &mut JitBuffer, dst: u8) {
    emit_rex_w(buf, 0, dst);
    buf.emit_u8(0xD3);
    emit_modrm(buf, 3, 5, dst);
}

fn emit_sar_reg64(buf: &mut JitBuffer, dst: u8) {
    emit_rex_w(buf, 0, dst);
    buf.emit_u8(0xD3);
    emit_modrm(buf, 3, 7, dst);
}

fn emit_shl_reg32(buf: &mut JitBuffer, dst: u8) {
    emit_rex_if(buf, 0, dst);
    buf.emit_u8(0xD3);
    emit_modrm(buf, 3, 4, dst);
}

fn emit_shr_reg32(buf: &mut JitBuffer, dst: u8) {
    emit_rex_if(buf, 0, dst);
    buf.emit_u8(0xD3);
    emit_modrm(buf, 3, 5, dst);
}

fn emit_sar_reg32(buf: &mut JitBuffer, dst: u8) {
    emit_rex_if(buf, 0, dst);
    buf.emit_u8(0xD3);
    emit_modrm(buf, 3, 7, dst);
}

fn emit_shl_imm64(buf: &mut JitBuffer, dst: u8, imm: u8) {
    emit_rex_w(buf, 0, dst);
    buf.emit_u8(0xC1);
    emit_modrm(buf, 3, 4, dst);
    buf.emit_u8(imm);
}

fn emit_shr_imm64(buf: &mut JitBuffer, dst: u8, imm: u8) {
    emit_rex_w(buf, 0, dst);
    buf.emit_u8(0xC1);
    emit_modrm(buf, 3, 5, dst);
    buf.emit_u8(imm);
}

fn emit_sar_imm64(buf: &mut JitBuffer, dst: u8, imm: u8) {
    emit_rex_w(buf, 0, dst);
    buf.emit_u8(0xC1);
    emit_modrm(buf, 3, 7, dst);
    buf.emit_u8(imm);
}

fn emit_shl_imm32(buf: &mut JitBuffer, dst: u8, imm: u8) {
    emit_rex_if(buf, 0, dst);
    buf.emit_u8(0xC1);
    emit_modrm(buf, 3, 4, dst);
    buf.emit_u8(imm);
}

fn emit_shr_imm32(buf: &mut JitBuffer, dst: u8, imm: u8) {
    emit_rex_if(buf, 0, dst);
    buf.emit_u8(0xC1);
    emit_modrm(buf, 3, 5, dst);
    buf.emit_u8(imm);
}

fn emit_sar_imm32(buf: &mut JitBuffer, dst: u8, imm: u8) {
    emit_rex_if(buf, 0, dst);
    buf.emit_u8(0xC1);
    emit_modrm(buf, 3, 7, dst);
    buf.emit_u8(imm);
}

fn emit_test_reg64(buf: &mut JitBuffer, r1: u8, r2: u8) {
    emit_rex_w(buf, r2, r1);
    buf.emit_u8(0x85);
    emit_modrm(buf, 3, r2, r1);
}

fn emit_cmp_reg64(buf: &mut JitBuffer, r1: u8, r2: u8) {
    emit_rex_w(buf, r2, r1);
    buf.emit_u8(0x39);
    emit_modrm(buf, 3, r2, r1);
}

fn emit_cmp_reg32(buf: &mut JitBuffer, r1: u8, r2: u8) {
    emit_rex_if(buf, r2, r1);
    buf.emit_u8(0x39);
    emit_modrm(buf, 3, r2, r1);
}

fn emit_je(buf: &mut JitBuffer, off: i32) {
    buf.emit_u8(0x0F);
    buf.emit_u8(0x84);
    buf.emit_u32(off as u32);
}

fn emit_jne(buf: &mut JitBuffer, off: i32) {
    buf.emit_u8(0x0F);
    buf.emit_u8(0x85);
    buf.emit_u32(off as u32);
}

fn emit_ja(buf: &mut JitBuffer, off: i32) {
    buf.emit_u8(0x0F);
    buf.emit_u8(0x87);
    buf.emit_u32(off as u32);
}

fn emit_jae(buf: &mut JitBuffer, off: i32) {
    buf.emit_u8(0x0F);
    buf.emit_u8(0x83);
    buf.emit_u32(off as u32);
}

fn emit_jb(buf: &mut JitBuffer, off: i32) {
    buf.emit_u8(0x0F);
    buf.emit_u8(0x82);
    buf.emit_u32(off as u32);
}

fn emit_jbe(buf: &mut JitBuffer, off: i32) {
    buf.emit_u8(0x0F);
    buf.emit_u8(0x86);
    buf.emit_u32(off as u32);
}

fn emit_jg(buf: &mut JitBuffer, off: i32) {
    buf.emit_u8(0x0F);
    buf.emit_u8(0x8F);
    buf.emit_u32(off as u32);
}

fn emit_jge(buf: &mut JitBuffer, off: i32) {
    buf.emit_u8(0x0F);
    buf.emit_u8(0x8D);
    buf.emit_u32(off as u32);
}

fn emit_jl(buf: &mut JitBuffer, off: i32) {
    buf.emit_u8(0x0F);
    buf.emit_u8(0x8C);
    buf.emit_u32(off as u32);
}

fn emit_jle(buf: &mut JitBuffer, off: i32) {
    buf.emit_u8(0x0F);
    buf.emit_u8(0x8E);
    buf.emit_u32(off as u32);
}

fn emit_jmp_rel32(buf: &mut JitBuffer, off: i32) {
    buf.emit_u8(0xE9);
    buf.emit_u32(off as u32);
}

fn emit_call_reg(buf: &mut JitBuffer, r: u8) {
    if need_rex(r) {
        buf.emit_u8(0x41);
    }
    buf.emit_u8(0xFF);
    emit_modrm(buf, 3, 2, r);
}

fn emit_ret(buf: &mut JitBuffer) {
    buf.emit_u8(0xC3);
}

fn emit_store_mem(buf: &mut JitBuffer, base: u8, off: i32, src: u8, size: u8) {
    match size {
        BPF_B => {
            emit_rex_if(buf, 0, src);
            buf.emit_u8(0x88);
            emit_modrm_disp(buf, src, base, off);
        }
        BPF_H => {
            buf.emit_u8(0x66);
            emit_rex_if(buf, 0, src);
            buf.emit_u8(0x89);
            emit_modrm_disp(buf, src, base, off);
        }
        BPF_W => {
            emit_rex_if(buf, 0, src);
            buf.emit_u8(0x89);
            emit_modrm_disp(buf, src, base, off);
        }
        BPF_DW => {
            emit_rex_w(buf, src, base);
            buf.emit_u8(0x89);
            emit_modrm_disp(buf, src, base, off);
        }
        _ => {}
    }
}

fn emit_load_mem(buf: &mut JitBuffer, dst: u8, base: u8, off: i32, size: u8) {
    match size {
        BPF_B => {
            emit_rex_if(buf, 0, dst);
            buf.emit_u8(0x0F);
            buf.emit_u8(0xB6);
            emit_modrm_disp(buf, dst, base, off);
        }
        BPF_H => {
            emit_rex_if(buf, 0, dst);
            buf.emit_u8(0x0F);
            buf.emit_u8(0xB7);
            emit_modrm_disp(buf, dst, base, off);
        }
        BPF_W => {
            emit_rex_if(buf, 0, dst);
            buf.emit_u8(0x8B);
            emit_modrm_disp(buf, dst, base, off);
        }
        BPF_DW => {
            emit_rex_w(buf, dst, base);
            buf.emit_u8(0x8B);
            emit_modrm_disp(buf, dst, base, off);
        }
        _ => {}
    }
}

fn emit_zext32(buf: &mut JitBuffer, r: u8) {
    emit_rex_if(buf, 0, r);
    buf.emit_u8(0x23);
    buf.emit_u8(0xC0 | ((r & 7) << 3) | (r & 7));
}

fn emit_divmod(buf: &mut JitBuffer, dst: u8, src: u8, is_div: bool, is_64: bool) {
    emit_push(buf, X86_RCX);

    if is_64 {
        emit_mov_reg64(buf, X86_R11, src);
        emit_mov_reg64(buf, X86_R10, dst);
        emit_xor_reg64(buf, dst, dst);
        emit_test_reg64(buf, X86_R11, X86_R11);
        let skip = buf.offset();
        emit_je(buf, 0);
        emit_mov_reg64(buf, X86_RAX, X86_R10);
        emit_xor_reg64(buf, X86_RDX, X86_RDX);
        emit_rex_w(buf, 0, X86_R11);
        buf.emit_u8(0xF7);
        emit_modrm(buf, 3, 6, X86_R11);
        emit_mov_reg64(buf, dst, if is_div { X86_RAX } else { X86_RDX });
        let after = buf.offset();
        unsafe {
            let ptr = buf.entry().add(skip) as *mut u8;
            let off = (after - skip - 6) as i32;
            core::ptr::copy_nonoverlapping(off.to_le_bytes().as_ptr(), ptr.add(2), 4);
        }
    } else {
        emit_mov_reg32(buf, X86_R11, src);
        emit_mov_reg32(buf, X86_R10, dst);
        emit_xor_reg32(buf, dst, dst);
        emit_test_reg64(buf, X86_R11, X86_R11);
        let skip = buf.offset();
        emit_je(buf, 0);
        emit_mov_reg32(buf, X86_RAX, X86_R10);
        emit_zext32(buf, X86_RAX);
        emit_xor_reg32(buf, X86_RDX, X86_RDX);
        emit_rex_if(buf, 0, X86_R11);
        buf.emit_u8(0xF7);
        emit_modrm(buf, 3, 6, X86_R11);
        emit_zext32(buf, if is_div { X86_RAX } else { X86_RDX });
        emit_mov_reg32(buf, dst, if is_div { X86_RAX } else { X86_RDX });
        let after = buf.offset();
        unsafe {
            let ptr = buf.entry().add(skip) as *mut u8;
            let off = (after - skip - 6) as i32;
            core::ptr::copy_nonoverlapping(off.to_le_bytes().as_ptr(), ptr.add(2), 4);
        }
    }

    emit_pop(buf, X86_RCX);
}

pub(crate) struct X86_64Backend;

impl JitBackend for X86_64Backend {
    fn emit_prologue(buf: &mut JitBuffer) -> usize {
        emit_push(buf, X86_RBP);
        emit_mov_reg64(buf, X86_RSP, X86_RBP);
        emit_push(buf, X86_RBX);
        emit_push(buf, X86_R13);
        emit_push(buf, X86_R14);
        emit_push(buf, X86_R15);
        buf.emit_u8(0x48);
        buf.emit_u8(0x81);
        buf.emit_u8(0xEC);
        buf.emit_u32(512);
        buf.emit_u8(0x48);
        buf.emit_u8(0x8D);
        buf.emit_u8(0x65);
        buf.emit_u8(0x00);
        emit_mov_reg64(buf, X86_RDI, X86_RBP);
        buf.offset()
    }

    fn emit_epilogue(buf: &mut JitBuffer) {
        buf.emit_u8(0x48);
        buf.emit_u8(0x81);
        buf.emit_u8(0xC4);
        buf.emit_u32(512);
        emit_pop(buf, X86_R15);
        emit_pop(buf, X86_R14);
        emit_pop(buf, X86_R13);
        emit_pop(buf, X86_RBX);
        emit_pop(buf, X86_RBP);
        emit_ret(buf);
    }

    fn emit_alu(buf: &mut JitBuffer, insn: &BpfInsn, is_64: bool) {
        let dst = bpf_to_x86(insn.dst_reg());
        let use_imm = (insn.code & BPF_X) == 0;
        let src = if use_imm {
            X86_RCX
        } else {
            bpf_to_x86(insn.src_reg())
        };

        if use_imm {
            let imm = insn.imm;
            if is_64 {
                if (0..256).contains(&imm) {
                    emit_mov_imm32(buf, dst, imm);
                } else {
                    emit_mov_imm64(buf, X86_RCX, insn.imm as u64);
                }
            } else {
                emit_mov_imm32(buf, X86_RCX, imm);
            }
        }

        match insn.alu_op() {
            BPF_ADD => {
                if is_64 {
                    emit_add_reg64(buf, dst, src);
                } else {
                    emit_add_reg32(buf, dst, src);
                    emit_zext32(buf, dst);
                }
            }
            BPF_SUB => {
                if is_64 {
                    emit_sub_reg64(buf, dst, src);
                } else {
                    emit_sub_reg32(buf, dst, src);
                    emit_zext32(buf, dst);
                }
            }
            BPF_MUL => {
                if is_64 {
                    emit_imul_reg64(buf, dst, src);
                } else {
                    emit_imul_reg32(buf, dst, src);
                    emit_zext32(buf, dst);
                }
            }
            BPF_DIV => {
                emit_divmod(buf, dst, src, true, is_64);
            }
            BPF_OR => {
                if is_64 {
                    emit_or_reg64(buf, dst, src);
                } else {
                    emit_or_reg32(buf, dst, src);
                    emit_zext32(buf, dst);
                }
            }
            BPF_AND => {
                if is_64 {
                    emit_and_reg64(buf, dst, src);
                } else {
                    emit_and_reg32(buf, dst, src);
                    emit_zext32(buf, dst);
                }
            }
            BPF_LSH => {
                if use_imm {
                    let shamt = (insn.imm as u8) & (if is_64 { 63 } else { 31 });
                    if is_64 {
                        emit_shl_imm64(buf, dst, shamt);
                    } else {
                        emit_shl_imm32(buf, dst, shamt);
                        emit_zext32(buf, dst);
                    }
                } else if is_64 {
                    emit_mov_reg64(buf, X86_RCX, src);
                    emit_shl_reg64(buf, dst);
                } else {
                    emit_mov_reg32(buf, X86_RCX, src);
                    emit_shl_reg32(buf, dst);
                    emit_zext32(buf, dst);
                }
            }
            BPF_RSH => {
                if use_imm {
                    let shamt = (insn.imm as u8) & (if is_64 { 63 } else { 31 });
                    if is_64 {
                        emit_shr_imm64(buf, dst, shamt);
                    } else {
                        emit_shr_imm32(buf, dst, shamt);
                        emit_zext32(buf, dst);
                    }
                } else if is_64 {
                    emit_mov_reg64(buf, X86_RCX, src);
                    emit_shr_reg64(buf, dst);
                } else {
                    emit_mov_reg32(buf, X86_RCX, src);
                    emit_shr_reg32(buf, dst);
                    emit_zext32(buf, dst);
                }
            }
            BPF_NEG => {
                if is_64 {
                    emit_neg_reg64(buf, dst);
                } else {
                    emit_neg_reg32(buf, dst);
                    emit_zext32(buf, dst);
                }
            }
            BPF_MOD => {
                emit_divmod(buf, dst, src, false, is_64);
            }
            BPF_XOR => {
                if is_64 {
                    emit_xor_reg64(buf, dst, src);
                } else {
                    emit_xor_reg32(buf, dst, src);
                    emit_zext32(buf, dst);
                }
            }
            BPF_MOV => {
                if is_64 {
                    if use_imm {
                        emit_mov_imm64(buf, dst, insn.imm as u64);
                    } else {
                        emit_mov_reg64(buf, dst, src);
                    }
                } else {
                    if use_imm {
                        emit_mov_imm32(buf, dst, insn.imm);
                    } else {
                        emit_mov_reg32(buf, dst, src);
                    }
                    emit_zext32(buf, dst);
                }
            }
            BPF_ARSH => {
                if use_imm {
                    let shamt = (insn.imm as u8) & (if is_64 { 63 } else { 31 });
                    if is_64 {
                        emit_sar_imm64(buf, dst, shamt);
                    } else {
                        emit_sar_imm32(buf, dst, shamt);
                        emit_zext32(buf, dst);
                    }
                } else if is_64 {
                    emit_mov_reg64(buf, X86_RCX, src);
                    emit_sar_reg64(buf, dst);
                } else {
                    emit_mov_reg32(buf, X86_RCX, src);
                    emit_sar_reg32(buf, dst);
                    emit_zext32(buf, dst);
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
                let off = offsets[target_pc] as isize - buf.offset() as isize - 5;
                emit_jmp_rel32(buf, off as i32);
            }
            return;
        }

        let dst = bpf_to_x86(insn.dst_reg());
        let use_imm = (insn.code & BPF_X) == 0;
        let src = if use_imm {
            X86_RCX
        } else {
            bpf_to_x86(insn.src_reg())
        };

        if use_imm {
            if is_64 {
                emit_mov_imm64(buf, X86_RCX, insn.imm as u64);
            } else {
                emit_mov_imm32(buf, X86_RCX, insn.imm);
            }
        }

        if is_64 {
            emit_cmp_reg64(buf, dst, src);
        } else {
            emit_cmp_reg32(buf, dst, src);
        }

        let target_pc = (pc as isize + 1 + insn.off as isize) as usize;
        let target_off = if target_pc < offsets.len() {
            (offsets[target_pc] as isize - buf.offset() as isize - 6) as i32
        } else {
            0
        };

        match op {
            BPF_JEQ => emit_je(buf, target_off),
            BPF_JGT => emit_ja(buf, target_off),
            BPF_JGE => emit_jae(buf, target_off),
            BPF_JSET => {
                if is_64 {
                    emit_test_reg64(buf, dst, src);
                } else {
                    emit_rex_if(buf, src, dst);
                    buf.emit_u8(0x85);
                    emit_modrm(buf, 3, src, dst);
                }
                emit_jne(buf, target_off);
            }
            BPF_JNE => emit_jne(buf, target_off),
            BPF_JSGT => emit_jg(buf, target_off),
            BPF_JSGE => emit_jge(buf, target_off),
            BPF_JLT => emit_jb(buf, target_off),
            BPF_JLE => emit_jbe(buf, target_off),
            BPF_JSLT => emit_jl(buf, target_off),
            BPF_JSLE => emit_jle(buf, target_off),
            _ => {}
        }
    }

    fn emit_st(buf: &mut JitBuffer, insn: &BpfInsn) {
        if insn.mode() != BPF_MEM {
            return;
        }
        let off = insn.off as i32;
        let imm = insn.imm as i64;
        if insn.size() == BPF_DW {
            emit_mov_imm64(buf, X86_RCX, imm as u64);
            emit_store_mem(buf, X86_RBP, off, X86_RCX, BPF_DW);
        } else {
            emit_mov_imm32(buf, X86_RCX, imm as i32);
            emit_store_mem(buf, X86_RBP, off, X86_RCX, insn.size());
        }
    }

    fn emit_stx(buf: &mut JitBuffer, insn: &BpfInsn) {
        if insn.mode() != BPF_MEM {
            return;
        }
        let off = insn.off as i32;
        let src = bpf_to_x86(insn.src_reg());
        let sz = if insn.size() == BPF_DW {
            BPF_DW
        } else {
            insn.size()
        };
        emit_store_mem(buf, X86_RBP, off, src, sz);
    }

    fn emit_ldx(buf: &mut JitBuffer, insn: &BpfInsn) {
        if insn.mode() != BPF_MEM {
            return;
        }
        let off = insn.off as i32;
        let base = bpf_to_x86(insn.src_reg());
        let dst = bpf_to_x86(insn.dst_reg());
        emit_load_mem(buf, dst, base, off, insn.size());
    }

    fn emit_ld_imm64(buf: &mut JitBuffer, insn: &BpfInsn, next_imm: i32) {
        let dst = bpf_to_x86(insn.dst_reg());
        let imm_lo = insn.imm as u64;
        let imm_hi = next_imm as u64;
        let val = (imm_hi << 32) | (imm_lo & 0xffffffff);
        emit_mov_imm64(buf, dst, val);
    }

    fn emit_call(buf: &mut JitBuffer, helper_fn: HelperFn) {
        emit_mov_imm64(buf, X86_RAX, helper_fn as usize as u64);
        emit_call_reg(buf, X86_RAX);
    }

    fn insn_size(insn: &BpfInsn) -> usize {
        let class = insn.class();
        let use_imm = (insn.code & BPF_X) == 0;

        match class {
            BPF_ALU | BPF_ALU64 => {
                let alu_op = insn.alu_op();
                let is_64 = class == BPF_ALU64;
                let load_size = if use_imm {
                    if alu_op == BPF_MOV && is_64 { 0 } else { 7 }
                } else {
                    0
                };
                let op_size = match alu_op {
                    BPF_DIV | BPF_MOD => 50,
                    BPF_MOV => {
                        if use_imm {
                            10
                        } else {
                            3
                        }
                    }
                    _ => 3 + 3,
                };
                load_size + op_size
            }
            BPF_JMP | BPF_JMP32 => {
                let op = insn.code & 0xf0;
                if op == BPF_EXIT || op == 0x80 {
                    16
                } else if insn.code == (BPF_JMP | BPF_JA) || insn.code == (BPF_JMP32 | BPF_JA) {
                    5
                } else {
                    let imm_size = if use_imm { 10 } else { 0 };
                    imm_size + 4 + 6
                }
            }
            BPF_ST => {
                if insn.size() == BPF_DW {
                    20
                } else {
                    12
                }
            }
            BPF_STX => 8,
            BPF_LDX => 8,
            BPF_LD if insn.is_ld_dw_imm() => 10,
            _ => 4,
        }
    }
}

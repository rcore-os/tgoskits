use super::{
    super::{
        HelperFn,
        bpf_insn::{
            BPF_ADD, BPF_ALU, BPF_ALU64, BPF_AND, BPF_ARSH, BPF_B, BPF_DIV, BPF_DW, BPF_END,
            BPF_EXIT, BPF_H, BPF_JA, BPF_JEQ, BPF_JGE, BPF_JGT, BPF_JLE, BPF_JLT, BPF_JMP,
            BPF_JMP32, BPF_JNE, BPF_JSET, BPF_JSGE, BPF_JSGT, BPF_JSLE, BPF_JSLT, BPF_LD, BPF_LDX,
            BPF_LSH, BPF_MEM, BPF_MOD, BPF_MOV, BPF_MUL, BPF_NEG, BPF_OR, BPF_RSH, BPF_ST, BPF_STX,
            BPF_SUB, BPF_W, BPF_X, BPF_XOR, BpfInsn,
        },
    },
    JitBackend, JitBuffer,
};

// ==========================================================================
// AArch64 register mapping
// ==========================================================================

// BPF R0  = x0  (return value / helper arg 1)
// BPF R1  = x1  (context pointer / helper arg 2)
// BPF R2  = x2  (helper arg 3)
// BPF R3  = x3  (helper arg 4)
// BPF R4  = x4  (helper arg 5)
// BPF R5  = x5  (caller-saved)
// BPF R6  = x19 (callee-saved)
// BPF R7  = x20 (callee-saved)
// BPF R8  = x21 (callee-saved)
// BPF R9  = x22 (callee-saved)
// BPF R10 = x25 (frame pointer base)
//
// Temps: x6, x7, x9, x10, x11, x12, x16, x17
// x8  = indirect result (not used)
// x29 = FP (callee-saved, saved for stack walking)
// x30 = LR (saved for return)
// xzr/sp = 31 (zero or stack pointer depending on context)

const A64_X0: u32 = 0;
const A64_X1: u32 = 1;
const A64_X2: u32 = 2;
const A64_X3: u32 = 3;
const A64_X4: u32 = 4;
const A64_X5: u32 = 5;
const A64_X6: u32 = 6;
const A64_X7: u32 = 7;
const A64_X8: u32 = 8;
const A64_X9: u32 = 9;
const A64_X10: u32 = 10;
const A64_X11: u32 = 11;
const A64_X12: u32 = 12;
const A64_X16: u32 = 16;
const A64_X17: u32 = 17;
const A64_X19: u32 = 19;
const A64_X20: u32 = 20;
const A64_X21: u32 = 21;
const A64_X22: u32 = 22;
const A64_X25: u32 = 25;
const A64_X29: u32 = 29;
const A64_X30: u32 = 30;
// x31 is SP or XZR depending on instruction context
const A64_SP: u32 = 31;
const A64_XZR: u32 = 31;

const BPF_STACK_SIZE: usize = 512;
/// Saved registers: x19-x22, x25 (5 regs) + x29, x30 = 7 regs, padded to 8
const CALLEE_SAVED_SIZE: usize = 64; // 8 registers * 8 bytes
const FRAME_SIZE: usize = BPF_STACK_SIZE + CALLEE_SAVED_SIZE;

fn bpf_to_a64(r: u8) -> u32 {
    match r {
        0 => A64_X0,
        1 => A64_X1,
        2 => A64_X2,
        3 => A64_X3,
        4 => A64_X4,
        5 => A64_X5,
        6 => A64_X19,
        7 => A64_X20,
        8 => A64_X21,
        9 => A64_X22,
        10 => A64_X25,
        _ => A64_XZR,
    }
}

// ==========================================================================
// AArch64 instruction encoding helpers
// ==========================================================================

/// ADD (shifted register): Xd = Xn + Xm
fn a64_add(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=1 | op=0 | S=0 | 01011 | shift=00 | 0 | Rm | imm6=000000 | Rn | Rd
    buf.emit_u32(0x8B00_0000 | (rm << 16) | (rn << 5) | rd);
}

/// ADD (shifted register, 32-bit): Wd = Wn + Wm
fn a64_addw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=0 | op=0 | S=0 | 01011 | shift=00 | 0 | Rm | imm6=000000 | Rn | Rd
    buf.emit_u32(0x0B00_0000 | (rm << 16) | (rn << 5) | rd);
}

/// SUB (shifted register): Xd = Xn - Xm
fn a64_sub(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=1 | op=1 | S=0 | 01011 | shift=00 | 0 | Rm | imm6=000000 | Rn | Rd
    buf.emit_u32(0xCB00_0000 | (rm << 16) | (rn << 5) | rd);
}

/// SUB (shifted register, 32-bit): Wd = Wn - Wm
fn a64_subw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=0 | op=1 | S=0 | 01011 | shift=00 | 0 | Rm | imm6=000000 | Rn | Rd
    buf.emit_u32(0x4B00_0000 | (rm << 16) | (rn << 5) | rd);
}

/// AND (shifted register): Xd = Xn & Xm
fn a64_and(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=1 | opc=00 | 01010 | shift=00 | N=0 | Rm | imm6=000000 | Rn | Rd
    buf.emit_u32(0x8A00_0000 | (rm << 16) | (rn << 5) | rd);
}

/// AND (shifted register, 32-bit): Wd = Wn & Wm
fn a64_andw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x0A00_0000 | (rm << 16) | (rn << 5) | rd);
}

/// ORR (shifted register): Xd = Xn | Xm
fn a64_orr(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=1 | opc=01 | 01010 | shift=00 | N=0 | Rm | imm6=000000 | Rn | Rd
    buf.emit_u32(0xAA00_0000 | (rm << 16) | (rn << 5) | rd);
}

/// ORR (shifted register, 32-bit): Wd = Wn | Wm
fn a64_orrw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x2A00_0000 | (rm << 16) | (rn << 5) | rd);
}

/// EOR (shifted register): Xd = Xn ^ Xm
fn a64_eor(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=1 | opc=10 | 01010 | shift=00 | N=0 | Rm | imm6=000000 | Rn | Rd
    buf.emit_u32(0xCA00_0000 | (rm << 16) | (rn << 5) | rd);
}

/// EOR (shifted register, 32-bit): Wd = Wn ^ Wm
fn a64_eorw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x4A00_0000 | (rm << 16) | (rn << 5) | rd);
}

/// ADD (immediate): Xd = Xn + #imm12
fn a64_addi(buf: &mut JitBuffer, rd: u32, rn: u32, imm12: u32) {
    // sf=1 | op=0 | S=0 | 100010 | sh=0 | imm12 | Rn | Rd
    buf.emit_u32(0x9100_0000 | ((imm12 & 0xFFF) << 10) | (rn << 5) | rd);
}

/// ADD (immediate, 32-bit): Wd = Wn + #imm12
fn a64_addiw(buf: &mut JitBuffer, rd: u32, rn: u32, imm12: u32) {
    buf.emit_u32(0x1100_0000 | ((imm12 & 0xFFF) << 10) | (rn << 5) | rd);
}

/// SUB (immediate): Xd = Xn - #imm12
fn a64_subi(buf: &mut JitBuffer, rd: u32, rn: u32, imm12: u32) {
    // sf=1 | op=1 | S=0 | 100010 | sh=0 | imm12 | Rn | Rd
    buf.emit_u32(0xD100_0000 | ((imm12 & 0xFFF) << 10) | (rn << 5) | rd);
}

/// SUB (immediate, 32-bit): Wd = Wn - #imm12
fn a64_subiw(buf: &mut JitBuffer, rd: u32, rn: u32, imm12: u32) {
    buf.emit_u32(0x5100_0000 | ((imm12 & 0xFFF) << 10) | (rn << 5) | rd);
}

/// MADD: Xd = Xa + Xn * Xm
fn a64_madd(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32, ra: u32) {
    // sf=1 | op31=00 | 11000 | 000 | Rm | 0 | Ra | Rn | Rd
    buf.emit_u32(0x9B00_0000 | (rm << 16) | (ra << 10) | (rn << 5) | rd);
}

/// MADD (32-bit): Wd = Wa + Wn * Wm
fn a64_maddw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32, ra: u32) {
    // sf=0 | op31=00 | 11000 | 000 | Rm | 0 | Ra | Rn | Rd
    buf.emit_u32(0x1B00_0000 | (rm << 16) | (ra << 10) | (rn << 5) | rd);
}

/// MUL: Xd = Xn * Xm  (alias of MADD with XZR)
fn a64_mul(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    a64_madd(buf, rd, rn, rm, A64_XZR);
}

/// MUL (32-bit): Wd = Wn * Wm
fn a64_mulw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    a64_maddw(buf, rd, rn, rm, A64_XZR);
}

/// SDIV: Xd = Xn / Xm (signed)
fn a64_sdiv(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=1 | opc=11 | 11010110 | Rm | 000011 | Rn | Rd
    buf.emit_u32(0x9AC0_0C00 | (rm << 16) | (rn << 5) | rd);
}

/// UDIV: Xd = Xn / Xm (unsigned)
fn a64_udiv(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=1 | opc=10 | 11010110 | Rm | 000010 | Rn | Rd
    buf.emit_u32(0x9AC0_0800 | (rm << 16) | (rn << 5) | rd);
}

/// UDIV (32-bit, unsigned)
fn a64_udivw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=0
    buf.emit_u32(0x1AC0_0800 | (rm << 16) | (rn << 5) | rd);
}

/// UBFM (used for LSL/LSR/zero-extend): Xd = Xn[imms:immr]
/// UBFM has sf | 1|0 | 100110 | N(1) | immr(6) | imms(6) | Rn | Rd
/// sf=1: 64-bit, sf=0: 32-bit, N=sf
fn a64_ubfm(buf: &mut JitBuffer, sf: u32, rd: u32, rn: u32, immr: u32, imms: u32) {
    let enc = (sf << 31)
        | (0b10 << 29)
        | (0b100110 << 23)
        | (sf << 22)
        | (immr << 16)
        | (imms << 10)
        | (rn << 5)
        | rd;
    buf.emit_u32(enc);
}

/// SBFM (used for ASR/sign-extend)
/// sf | 0|0 | 100110 | N(sf) | immr | imms | Rn | Rd
fn a64_sbfm(buf: &mut JitBuffer, sf: u32, rd: u32, rn: u32, immr: u32, imms: u32) {
    let enc = (sf << 31)
        | (0b00 << 29)
        | (0b100110 << 23)
        | (sf << 22)
        | (immr << 16)
        | (imms << 10)
        | (rn << 5)
        | rd;
    buf.emit_u32(enc);
}

/// LSL (immediate): Xd = Xn << sh (UBFM alias)
fn a64_lsl(buf: &mut JitBuffer, rd: u32, rn: u32, sh: u32) {
    let immr = ((-(sh as i32)) & 0x3F) as u32;
    let imms = 63 - sh;
    a64_ubfm(buf, 1, rd, rn, immr, imms);
}

/// LSL (immediate, 32-bit)
fn a64_lslw(buf: &mut JitBuffer, rd: u32, rn: u32, sh: u32) {
    let immr = ((-(sh as i32)) & 0x1F) as u32;
    let imms = 31 - sh;
    a64_ubfm(buf, 0, rd, rn, immr, imms);
}

/// LSR (immediate): Xd = Xn >> sh (UBFM alias)
fn a64_lsr(buf: &mut JitBuffer, rd: u32, rn: u32, sh: u32) {
    a64_ubfm(buf, 1, rd, rn, sh, 63);
}

/// LSR (immediate, 32-bit)
fn a64_lsrw(buf: &mut JitBuffer, rd: u32, rn: u32, sh: u32) {
    a64_ubfm(buf, 0, rd, rn, sh, 31);
}

/// ASR (immediate): Xd = Xn >>> sh (SBFM alias)
fn a64_asr(buf: &mut JitBuffer, rd: u32, rn: u32, sh: u32) {
    a64_sbfm(buf, 1, rd, rn, sh, 63);
}

/// ASR (immediate, 32-bit)
fn a64_asrw(buf: &mut JitBuffer, rd: u32, rn: u32, sh: u32) {
    a64_sbfm(buf, 0, rd, rn, sh, 31);
}

/// LSL (register): Xd = Xn << Xm
fn a64_lslv(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=1 | opc=10 | 11010110 | Rm | 0010 00 | Rn | Rd
    buf.emit_u32(0x9AC0_2000 | (rm << 16) | (rn << 5) | rd);
}

/// LSL (register, 32-bit)
fn a64_lslvw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=0
    buf.emit_u32(0x1AC0_2000 | (rm << 16) | (rn << 5) | rd);
}

/// LSR (register): Xd = Xn >> Xm
fn a64_lsrv(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=1 | opc=01 | 11010110 | Rm | 0010 01 | Rn | Rd
    buf.emit_u32(0x9AC0_2400 | (rm << 16) | (rn << 5) | rd);
}

/// LSR (register, 32-bit)
fn a64_lsrvw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x1AC0_2400 | (rm << 16) | (rn << 5) | rd);
}

/// ASR (register): Xd = Xn >>> Xm
fn a64_asrv(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    // sf=1 | opc=00 | 11010110 | Rm | 0010 10 | Rn | Rd
    buf.emit_u32(0x9AC0_2800 | (rm << 16) | (rn << 5) | rd);
}

/// ASR (register, 32-bit)
fn a64_asrvw(buf: &mut JitBuffer, rd: u32, rn: u32, rm: u32) {
    buf.emit_u32(0x1AC0_2800 | (rm << 16) | (rn << 5) | rd);
}

/// MOV (register): Xd = Xn  (alias of ORR with XZR)
fn a64_mov(buf: &mut JitBuffer, rd: u32, rn: u32) {
    a64_orr(buf, rd, A64_XZR, rn);
}

/// MOV (register, 32-bit): Wd = Wn
fn a64_movw(buf: &mut JitBuffer, rd: u32, rn: u32) {
    a64_orrw(buf, rd, A64_XZR, rn);
}

/// REV: Xd = ByteReverse(Xn) — reverse byte order in 64-bit register
fn a64_rev64(buf: &mut JitBuffer, rd: u32, rn: u32) {
    // dp_1src: sf=1, opcode2=00000, opcode=000011, field=0000
    buf.emit_u32(0xDAC0_0C00 | (rn << 5) | rd);
}

/// REV32: Wd = ByteReverse(Wn) — reverse byte order in 32-bit word
fn a64_rev32(buf: &mut JitBuffer, rd: u32, rn: u32) {
    // dp_1src: sf=0, opcode2=00000, opcode=000010, field=0000
    buf.emit_u32(0x5AC0_0800 | (rn << 5) | rd);
}

/// REV16: Wd = ReverseHalfwords(Wn) — reverse bytes in each 16-bit halfword
fn a64_rev16(buf: &mut JitBuffer, rd: u32, rn: u32) {
    // dp_1src: sf=0, opcode2=00000, opcode=000001, field=0000
    buf.emit_u32(0x5AC0_0400 | (rn << 5) | rd);
}

/// MOVZ: Xd = imm16 << (hw * 16), zeroing other bits
fn a64_movz(buf: &mut JitBuffer, rd: u32, imm16: u32, hw: u32) {
    // sf=1 | 0 | 0 | 100101 | hw(2) | imm16(16) | Rd(5)
    let enc = (1 << 31) | (0b100101 << 23) | ((hw & 3) << 21) | ((imm16 & 0xFFFF) << 5) | rd;
    buf.emit_u32(enc);
}

/// MOVK: Xd[hw*16+15:hw*16] = imm16, preserving other bits
fn a64_movk(buf: &mut JitBuffer, rd: u32, imm16: u32, hw: u32) {
    // sf=1 | 1 | 1 | 100101 | hw(2) | imm16(16) | Rd(5)
    let enc = (1 << 31)
        | (0b11 << 29)
        | (0b100101 << 23)
        | ((hw & 3) << 21)
        | ((imm16 & 0xFFFF) << 5)
        | rd;
    buf.emit_u32(enc);
}

/// LDR (64-bit): Xt = [Xn + #offset]  (offset is scaled by 8, no scaling in our encoding)
fn a64_ldr(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    // [31:30] 11 | [29:28] 01 | [27:24] 1101 | [23:22] depends on variant
    // For unsigned offset: 11 01 1101 0 1 0 imm12(12) Rn(5) Rt(5)
    let imm12 = off as u32;
    buf.emit_u32(0xF940_0000 | ((imm12 & 0xFFF) << 10) | (rn << 5) | rt);
}

/// LDR (32-bit, zero-extending): Wt = [Xn + #offset]
fn a64_ldrw(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    // 10 01 1101 0 1 0 imm12(12) Rn(5) Rt(5)
    buf.emit_u32(0xB940_0000 | ((off as u32 & 0xFFF) << 10) | (rn << 5) | rt);
}

/// LDRH (16-bit, zero-extending): Wt = [Xn + #offset]
fn a64_ldrh(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    // 01 01 1101 0 1 0 imm12(12) Rn(5) Rt(5)  (imm12 is byte offset)
    buf.emit_u32(0x7940_0000 | ((off as u32 & 0xFFF) << 10) | (rn << 5) | rt);
}

/// LDRB (8-bit, zero-extending): Wt = [Xn + #offset]
fn a64_ldrb(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    // 00 01 1101 0 1 0 imm12(12) Rn(5) Rt(5)
    buf.emit_u32(0x3940_0000 | ((off as u32 & 0xFFF) << 10) | (rn << 5) | rt);
}

/// STR (64-bit): [Xn + #offset] = Xt
fn a64_str(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    // 11 01 1101 0 0 0 imm12(12) Rn(5) Rt(5)
    buf.emit_u32(0xF900_0000 | ((off as u32 & 0xFFF) << 10) | (rn << 5) | rt);
}

/// STR (32-bit): [Xn + #offset] = Wt
fn a64_strw(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    buf.emit_u32(0xB900_0000 | ((off as u32 & 0xFFF) << 10) | (rn << 5) | rt);
}

/// STRH (16-bit): [Xn + #offset] = Wt
fn a64_strh(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    buf.emit_u32(0x7900_0000 | ((off as u32 & 0xFFF) << 10) | (rn << 5) | rt);
}

/// STRB (8-bit): [Xn + #offset] = Wt
fn a64_strb(buf: &mut JitBuffer, rt: u32, rn: u32, off: i32) {
    buf.emit_u32(0x3900_0000 | ((off as u32 & 0xFFF) << 10) | (rn << 5) | rt);
}

/// STP (store pair): [Xn + #imm] = Xt1, [Xn + #imm + 8] = Xt2
/// imm must be in range [-512, 504] and 8-byte aligned
fn a64_stp_pre(buf: &mut JitBuffer, rt1: u32, rt2: u32, rn: u32, imm: i32) {
    // 10 1 0 100 0 1 | imm7(7) | Rt2(5) | Rn(5) | Rt1(5)
    // imm7 is imm / 8, signed
    let imm7 = ((imm / 8) & 0x7F) as u32;
    buf.emit_u32(0xA980_0000 | (imm7 << 15) | (rt2 << 10) | (rn << 5) | rt1);
}

/// LDP (load pair, post-index): Xt1, Xt2 = [Xn], Xn += imm
fn a64_ldp_post(buf: &mut JitBuffer, rt1: u32, rt2: u32, rn: u32, imm: i32) {
    // 10 1 0 100 0 1 | imm7(7) | Rt2(5) | Rn(5) | Rt1(5)
    // post-index variant: 10 1 0 100 0 0 | imm7 | Rt2 | Rn | Rt1
    let imm7 = ((imm / 8) & 0x7F) as u32;
    buf.emit_u32(0xA8C0_0000 | (imm7 << 15) | (rt2 << 10) | (rn << 5) | rt1);
}

/// ADR: Xd = PC + imm  (imm is a signed 21-bit value)
fn a64_adr(buf: &mut JitBuffer, rd: u32, imm: i32) {
    let imm = imm as u32;
    let immlo = imm & 3;
    let immhi = (imm >> 2) & 0x7FFFF;
    // 0 | immhi(19) | 1 0000 | immlo(2) | Rd(5)
    buf.emit_u32((immhi << 5) | (immlo << 29) | (0x10 << 24) | rd);
}

/// B (unconditional branch): PC += imm26 * 4
fn a64_b(buf: &mut JitBuffer, imm: i32) {
    let imm26 = ((imm as u32) & 0x03FF_FFFF) >> 2;
    buf.emit_u32(0x1400_0000 | imm26);
}

/// B.cond: if condition, PC += imm19 * 4
fn a64_bcond(buf: &mut JitBuffer, cond: u32, imm: i32) {
    let imm19 = ((imm as u32) & 0x7FFFF) >> 2;
    buf.emit_u32(0x5400_0000 | (imm19 << 5) | cond);
}

/// RET: return to address in X30 (LR)
fn a64_ret(buf: &mut JitBuffer) {
    // 1101011 0010 11111 000000 | 11110 | 00000
    buf.emit_u32(0xD65F_03C0);
}

/// BR: unconditional branch to register Xn
fn a64_br(buf: &mut JitBuffer, rn: u32) {
    // 1101011 0000 11111 000000 | Rn | 00000
    buf.emit_u32(0xD61F_0000 | (rn << 5));
}

/// BLR: call function at Xn, LR = return address
fn a64_blr(buf: &mut JitBuffer, rn: u32) {
    // 1101011 0001 11111 000000 | Rn | 00000
    buf.emit_u32(0xD63F_0000 | (rn << 5));
}

/// NOP
fn a64_nop(buf: &mut JitBuffer) {
    buf.emit_u32(0xD503_201F);
}

/// TST: Xn & Xm, set flags (ANDS with XZR)
fn a64_tst(buf: &mut JitBuffer, rn: u32, rm: u32) {
    // sf=1 | opc=11 | 01010 | shift=00 | N=0 | Rm | imm6=000000 | Rn | XZR
    buf.emit_u32(0xEA00_0000 | (rm << 16) | (rn << 5) | A64_XZR);
}

/// TST (32-bit)
fn a64_tstw(buf: &mut JitBuffer, rn: u32, rm: u32) {
    buf.emit_u32(0x6A00_0000 | (rm << 16) | (rn << 5) | A64_XZR);
}

/// CMP: Xn - Xm, set flags (SUBS with XZR)
fn a64_cmp(buf: &mut JitBuffer, rn: u32, rm: u32) {
    // SUBS XZR, Xn, Xm
    // sf=1 | op=1 | S=1 | 01011 | shift=00 | 0 | Rm | imm6=000000 | Rn | XZR
    buf.emit_u32(0xEB00_0000 | (rm << 16) | (rn << 5) | A64_XZR);
}

/// CMP (32-bit)
fn a64_cmpw(buf: &mut JitBuffer, rn: u32, rm: u32) {
    buf.emit_u32(0x6B00_0000 | (rm << 16) | (rn << 5) | A64_XZR);
}

// ==========================================================================
// Condition codes
// ==========================================================================
const COND_EQ: u32 = 0b0000;
const COND_NE: u32 = 0b0001;
const COND_HS: u32 = 0b0010; // unsigned >=
const COND_LO: u32 = 0b0011; // unsigned <
const COND_MI: u32 = 0b0100;
const COND_PL: u32 = 0b0101;
const COND_VS: u32 = 0b0110;
const COND_VC: u32 = 0b0111;
const COND_HI: u32 = 0b1000; // unsigned >
const COND_LS: u32 = 0b1001; // unsigned <=
const COND_GE: u32 = 0b1010; // signed >=
const COND_LT: u32 = 0b1011; // signed <
const COND_GT: u32 = 0b1100; // signed >
const COND_LE: u32 = 0b1101; // signed <=

// ==========================================================================
// Higher-level codegen helpers
// ==========================================================================

/// Load a 64-bit immediate into a register using up to 4 MOVZ/MOVK instructions
fn emit_load_imm64(buf: &mut JitBuffer, rd: u32, val: u64) {
    // Handle small values inline
    if val == 0 {
        a64_mov(buf, rd, A64_XZR);
        return;
    }
    if val <= 0xFFFF {
        a64_movz(buf, rd, val as u32, 0);
        return;
    }
    let mut first = true;
    for hw in 0..4u32 {
        let chunk = ((val >> (hw * 16)) & 0xFFFF) as u32;
        if first {
            a64_movz(buf, rd, chunk, hw);
            first = false;
        } else if chunk != 0 {
            a64_movk(buf, rd, chunk, hw);
        }
    }
}

/// Load a 32-bit signed immediate into a register
fn emit_load_imm32(buf: &mut JitBuffer, rd: u32, val: i32) {
    if val == 0 {
        a64_movw(buf, rd, A64_XZR);
        return;
    }
    emit_load_imm64(buf, rd, val as u64);
}

/// Load a 64-bit immediate with NOP padding to 24 bytes (6 instructions)
fn emit_load_imm64_padded(buf: &mut JitBuffer, rd: u32, val: u64) {
    let start = buf.offset();
    emit_load_imm64(buf, rd, val);
    let emitted = buf.offset() - start;
    // Pad to 24 bytes (6 instructions)
    let pad = if emitted < 24 { (24 - emitted) / 4 } else { 0 };
    for _ in 0..pad {
        a64_nop(buf);
    }
}

/// Compute rd = rn + off, handling the case where off doesn't fit in 12-bit immediate
fn emit_add_offset(buf: &mut JitBuffer, rd: u32, rn: u32, off: i32) {
    if off >= 0 && (off as u32) < 4096 {
        a64_addi(buf, rd, rn, off as u32);
    } else if off < 0 && off > -4096 {
        a64_subi(buf, rd, rn, (-off) as u32);
    } else {
        emit_load_imm64(buf, A64_X6, off as u64);
        a64_add(buf, rd, rn, A64_X6);
    }
}

/// Patch a 32-bit value at a given offset in the buffer
unsafe fn patch_u32(buf: &JitBuffer, offset: usize, val: u32) {
    let ptr = buf.entry().add(offset) as *mut u32;
    *ptr = val.to_le();
}

// ==========================================================================
// JIT Backend implementation
// ==========================================================================

pub(crate) struct Aarch64Backend;

impl JitBackend for Aarch64Backend {
    fn emit_prologue(buf: &mut JitBuffer) -> usize {
        // Save frame pointer and link register
        a64_stp_pre(buf, A64_X29, A64_X30, A64_SP, -16);
        // Save callee-saved BPF registers: x19-x22, x25
        a64_stp_pre(buf, A64_X19, A64_X20, A64_SP, -16);
        a64_stp_pre(buf, A64_X21, A64_X22, A64_SP, -16);
        a64_stp_pre(buf, A64_X25, A64_XZR, A64_SP, -16);
        // Allocate BPF stack space
        a64_subi(buf, A64_SP, A64_SP, BPF_STACK_SIZE as u32);
        // x25 = frame pointer base = SP + BPF_STACK_SIZE + 64
        a64_addi(
            buf,
            A64_X25,
            A64_SP,
            (BPF_STACK_SIZE + CALLEE_SAVED_SIZE) as u32,
        );
        // Move context pointer: x0 → x1 (BPF R1), x0 (BPF R0) = 0
        a64_mov(buf, A64_X1, A64_X0);
        a64_mov(buf, A64_X0, A64_XZR);
        buf.offset()
    }

    fn emit_epilogue(buf: &mut JitBuffer) {
        // Deallocate BPF stack
        a64_addi(buf, A64_SP, A64_SP, BPF_STACK_SIZE as u32);
        // Restore callee-saved registers
        a64_ldp_post(buf, A64_X25, A64_XZR, A64_SP, 16);
        a64_ldp_post(buf, A64_X21, A64_X22, A64_SP, 16);
        a64_ldp_post(buf, A64_X19, A64_X20, A64_SP, 16);
        a64_ldp_post(buf, A64_X29, A64_X30, A64_SP, 16);
        // Return
        a64_ret(buf);
    }

    fn emit_alu(buf: &mut JitBuffer, insn: &BpfInsn, is_64: bool) {
        let dst = bpf_to_a64(insn.dst_reg());
        let use_imm = (insn.code & BPF_X) == 0;
        let src = if use_imm {
            A64_X6
        } else {
            bpf_to_a64(insn.src_reg())
        };
        if use_imm {
            if is_64 {
                emit_load_imm64(buf, A64_X6, insn.imm as u64);
            } else {
                emit_load_imm32(buf, A64_X6, insn.imm);
            }
        }

        match insn.alu_op() {
            BPF_ADD => {
                if is_64 {
                    a64_add(buf, dst, dst, src);
                } else {
                    a64_addw(buf, dst, dst, src);
                }
            }
            BPF_SUB => {
                if is_64 {
                    a64_sub(buf, dst, dst, src);
                } else {
                    a64_subw(buf, dst, dst, src);
                }
            }
            BPF_MUL => {
                if is_64 {
                    a64_mul(buf, dst, dst, src);
                } else {
                    a64_mulw(buf, dst, dst, src);
                }
            }
            BPF_DIV => {
                // Division by zero: set result to 0
                let skip = buf.offset();
                a64_cmp(buf, src, A64_XZR);
                a64_bcond(buf, COND_EQ, 0); // patched below
                if is_64 {
                    a64_udiv(buf, dst, dst, src);
                } else {
                    a64_udivw(buf, dst, dst, src);
                }
                let end_div = buf.offset();
                a64_b(buf, 8);
                // Zero result (skip target)
                a64_mov(buf, dst, A64_XZR);
                let end_zero = buf.offset();
                unsafe {
                    // Patch the B.EQ: offset from skip to end_div (skip the branch)
                    let beq_off = (end_div - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((beq_off >> 2) << 5) | COND_EQ);
                }
                assert_eq!(end_zero - end_div, 8);
            }
            BPF_OR => {
                if is_64 {
                    a64_orr(buf, dst, dst, src);
                } else {
                    a64_orrw(buf, dst, dst, src);
                }
            }
            BPF_AND => {
                if is_64 {
                    a64_and(buf, dst, dst, src);
                } else {
                    a64_andw(buf, dst, dst, src);
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
                        a64_lsl(buf, dst, dst, shamt);
                    } else {
                        a64_lslw(buf, dst, dst, shamt);
                    }
                } else if is_64 {
                    a64_lslv(buf, dst, dst, src);
                } else {
                    a64_lslvw(buf, dst, dst, src);
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
                        a64_lsr(buf, dst, dst, shamt);
                    } else {
                        a64_lsrw(buf, dst, dst, shamt);
                    }
                } else if is_64 {
                    a64_lsrv(buf, dst, dst, src);
                } else {
                    a64_lsrvw(buf, dst, dst, src);
                }
            }
            BPF_NEG => {
                if is_64 {
                    a64_sub(buf, dst, A64_XZR, dst);
                } else {
                    a64_subw(buf, dst, A64_XZR, dst);
                }
            }
            BPF_MOD => {
                // Division by zero: result = dst (unchanged in BPF for MOD)
                let skip = buf.offset();
                a64_cmp(buf, src, A64_XZR);
                a64_bcond(buf, COND_EQ, 0); // patched below
                // UDIV temp = dst / src; MSUB dst = dst - temp * src
                if is_64 {
                    a64_udiv(buf, A64_X7, dst, src); // X7 = dst / src
                    a64_madd(buf, A64_X7, A64_X7, src, A64_XZR); // X7 = X7 * src
                    a64_sub(buf, dst, dst, A64_X7); // dst = dst - (dst/src)*src
                } else {
                    a64_udivw(buf, A64_X7, dst, src);
                    a64_maddw(buf, A64_X7, A64_X7, src, A64_XZR);
                    a64_subw(buf, dst, dst, A64_X7);
                }
                let end = buf.offset();
                unsafe {
                    let beq_off = (end - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((beq_off >> 2) << 5) | COND_EQ);
                }
            }
            BPF_XOR => {
                if is_64 {
                    a64_eor(buf, dst, dst, src);
                } else {
                    a64_eorw(buf, dst, dst, src);
                }
            }
            BPF_MOV => {
                if is_64 {
                    a64_mov(buf, dst, src);
                } else {
                    a64_movw(buf, dst, src);
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
                        a64_asr(buf, dst, dst, shamt);
                    } else {
                        a64_asrw(buf, dst, dst, shamt);
                    }
                } else if is_64 {
                    a64_asrv(buf, dst, dst, src);
                } else {
                    a64_asrvw(buf, dst, dst, src);
                }
            }
            BPF_END => {
                // BPF_TO_BE: byte swap to big-endian (AArch64 is little-endian native)
                let to_be = (insn.code & BPF_X) != 0;
                match (to_be, insn.imm) {
                    (true, 16) => {
                        // 16-bit: REV16 Wd, Wn — swap bytes in each 16-bit halfword
                        a64_rev16(buf, dst, dst);
                    }
                    (true, 32) => {
                        // 32-bit: REV32 Wd, Wn — swap bytes in 32-bit word
                        a64_rev32(buf, dst, dst);
                    }
                    (true, 64) => {
                        // 64-bit: REV Xd, Xn — swap all 8 bytes
                        a64_rev64(buf, dst, dst);
                    }
                    // BPF_TO_LE on AArch64: no-op (native little-endian)
                    _ => {}
                }
            }
            _ => {}
        }

        // For 32-bit ALU ops (except ARSH, MOV), result is already zero-extended
        // by the W-form instruction. No explicit zext needed on AArch64.
    }

    fn emit_jmp(buf: &mut JitBuffer, insn: &BpfInsn, offsets: &[usize], pc: usize, is_64: bool) {
        let op = insn.code & 0xf0;

        // Unconditional jump (BPF_JA)
        if insn.code == (BPF_JMP | BPF_JA) || insn.code == (BPF_JMP32 | BPF_JA) {
            let target_pc = (pc as isize + 1 + insn.off as isize) as usize;
            if target_pc < offsets.len() {
                let target_offset = offsets[target_pc] as isize - buf.offset() as isize;
                // Load target offset and branch
                emit_load_imm64_padded(buf, A64_X16, target_offset as u64);
                // ADR x17, . ; ADD x17, x17, x16 ; BR x17
                a64_adr(buf, A64_X17, 0);
                a64_add(buf, A64_X17, A64_X17, A64_X16);
                a64_br(buf, A64_X17);
            }
            return;
        }

        // BPF_CALL is handled in compile()
        if op == 0x80 {
            return;
        }

        let dst = bpf_to_a64(insn.dst_reg());
        let use_imm = (insn.code & BPF_X) == 0;
        let src_reg = if use_imm {
            A64_X6
        } else {
            bpf_to_a64(insn.src_reg())
        };

        if use_imm {
            if is_64 {
                emit_load_imm64(buf, A64_X6, insn.imm as u64);
            } else {
                emit_load_imm32(buf, A64_X6, insn.imm);
            }
        }

        // Compare and set flags
        if is_64 {
            a64_cmp(buf, dst, src_reg);
        } else {
            a64_cmpw(buf, dst, src_reg);
        }

        let target_pc = (pc as isize + 1 + insn.off as isize) as usize;

        // Pattern: conditional branch to skip the jump, then jump to target
        fn emit_jump_to_target(buf: &mut JitBuffer, offsets: &[usize], target_pc: usize) {
            let target_offset = offsets[target_pc] as isize - buf.offset() as isize;
            emit_load_imm64_padded(buf, A64_X16, target_offset as u64);
            a64_adr(buf, A64_X17, 0);
            a64_add(buf, A64_X17, A64_X17, A64_X16);
            a64_br(buf, A64_X17);
        }

        match op {
            BPF_JEQ => {
                // Jump to target if dst == src → skip jump if dst != src
                let skip = buf.offset();
                a64_bcond(buf, COND_NE, 0); // patched
                emit_jump_to_target(buf, offsets, target_pc);
                let end = buf.offset();
                unsafe {
                    let bne_off = (end - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((bne_off >> 2) << 5) | COND_NE);
                }
            }
            BPF_JGT => {
                // Jump if dst > src (unsigned) → skip jump if dst <= src
                let skip = buf.offset();
                a64_bcond(buf, COND_LS, 0);
                emit_jump_to_target(buf, offsets, target_pc);
                let end = buf.offset();
                unsafe {
                    let bls_off = (end - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((bls_off >> 2) << 5) | COND_LS);
                }
            }
            BPF_JGE => {
                // Jump if dst >= src (unsigned) → skip jump if dst < src
                let skip = buf.offset();
                a64_bcond(buf, COND_LO, 0);
                emit_jump_to_target(buf, offsets, target_pc);
                let end = buf.offset();
                unsafe {
                    let blo_off = (end - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((blo_off >> 2) << 5) | COND_LO);
                }
            }
            BPF_JSET => {
                // Jump if dst & src != 0 → skip jump if dst & src == 0
                if is_64 {
                    a64_tst(buf, dst, src_reg);
                } else {
                    a64_tstw(buf, dst, src_reg);
                }
                let skip = buf.offset();
                a64_bcond(buf, COND_EQ, 0);
                emit_jump_to_target(buf, offsets, target_pc);
                let end = buf.offset();
                unsafe {
                    let beq_off = (end - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((beq_off >> 2) << 5) | COND_EQ);
                }
            }
            BPF_JNE => {
                let skip = buf.offset();
                a64_bcond(buf, COND_EQ, 0);
                emit_jump_to_target(buf, offsets, target_pc);
                let end = buf.offset();
                unsafe {
                    let beq_off = (end - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((beq_off >> 2) << 5) | COND_EQ);
                }
            }
            BPF_JSGT => {
                // Signed >
                let skip = buf.offset();
                a64_bcond(buf, COND_LE, 0);
                emit_jump_to_target(buf, offsets, target_pc);
                let end = buf.offset();
                unsafe {
                    let ble_off = (end - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((ble_off >> 2) << 5) | COND_LE);
                }
            }
            BPF_JSGE => {
                // Signed >=
                let skip = buf.offset();
                a64_bcond(buf, COND_LT, 0);
                emit_jump_to_target(buf, offsets, target_pc);
                let end = buf.offset();
                unsafe {
                    let blt_off = (end - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((blt_off >> 2) << 5) | COND_LT);
                }
            }
            BPF_JLT => {
                // Unsigned <
                let skip = buf.offset();
                a64_bcond(buf, COND_HS, 0);
                emit_jump_to_target(buf, offsets, target_pc);
                let end = buf.offset();
                unsafe {
                    let bhs_off = (end - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((bhs_off >> 2) << 5) | COND_HS);
                }
            }
            BPF_JLE => {
                // Unsigned <=
                let skip = buf.offset();
                a64_bcond(buf, COND_HI, 0);
                emit_jump_to_target(buf, offsets, target_pc);
                let end = buf.offset();
                unsafe {
                    let bhi_off = (end - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((bhi_off >> 2) << 5) | COND_HI);
                }
            }
            BPF_JSLT => {
                // Signed <
                let skip = buf.offset();
                a64_bcond(buf, COND_GE, 0);
                emit_jump_to_target(buf, offsets, target_pc);
                let end = buf.offset();
                unsafe {
                    let bge_off = (end - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((bge_off >> 2) << 5) | COND_GE);
                }
            }
            BPF_JSLE => {
                // Signed <=
                let skip = buf.offset();
                a64_bcond(buf, COND_GT, 0);
                emit_jump_to_target(buf, offsets, target_pc);
                let end = buf.offset();
                unsafe {
                    let bgt_off = (end - skip) as u32;
                    patch_u32(buf, skip, 0x5400_0000 | ((bgt_off >> 2) << 5) | COND_GT);
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
        let base = bpf_to_a64(insn.dst_reg());
        let adjusted_off = if base == A64_X25 {
            off - CALLEE_SAVED_SIZE as i32
        } else {
            off
        };
        // Classic BPF: ST [dst + off] = imm
        // eBPF: ST [dst + off] = imm (only BPF_ST size variants exist)
        emit_add_offset(buf, A64_X7, base, adjusted_off);
        let val = insn.imm as u64;
        match insn.size() {
            BPF_B => {
                emit_load_imm32(buf, A64_X6, val as i32);
                a64_strb(buf, A64_X6, A64_X7, 0);
            }
            BPF_H => {
                emit_load_imm32(buf, A64_X6, val as i32);
                a64_strh(buf, A64_X6, A64_X7, 0);
            }
            BPF_W => {
                emit_load_imm32(buf, A64_X6, val as i32);
                a64_strw(buf, A64_X6, A64_X7, 0);
            }
            BPF_DW => {
                emit_load_imm64(buf, A64_X6, val);
                a64_str(buf, A64_X6, A64_X7, 0);
            }
            _ => {}
        }
    }

    fn emit_stx(buf: &mut JitBuffer, insn: &BpfInsn) {
        if insn.mode() != BPF_MEM {
            return;
        }
        let off = insn.off as i32;
        let src = bpf_to_a64(insn.src_reg());
        let base = bpf_to_a64(insn.dst_reg());
        let adjusted_off = if base == A64_X25 {
            off - CALLEE_SAVED_SIZE as i32
        } else {
            off
        };
        emit_add_offset(buf, A64_X7, base, adjusted_off);
        match insn.size() {
            BPF_B => a64_strb(buf, src, A64_X7, 0),
            BPF_H => a64_strh(buf, src, A64_X7, 0),
            BPF_W => a64_strw(buf, src, A64_X7, 0),
            BPF_DW => a64_str(buf, src, A64_X7, 0),
            _ => {}
        }
    }

    fn emit_ldx(buf: &mut JitBuffer, insn: &BpfInsn) {
        if insn.mode() != BPF_MEM {
            return;
        }
        let off = insn.off as i32;
        let src = bpf_to_a64(insn.src_reg());
        let dst = bpf_to_a64(insn.dst_reg());
        let adjusted_off = if src == A64_X25 {
            off - CALLEE_SAVED_SIZE as i32
        } else {
            off
        };
        emit_add_offset(buf, A64_X7, src, adjusted_off);
        match insn.size() {
            BPF_B => a64_ldrb(buf, dst, A64_X7, 0),
            BPF_H => a64_ldrh(buf, dst, A64_X7, 0),
            BPF_W => a64_ldrw(buf, dst, A64_X7, 0),
            BPF_DW => a64_ldr(buf, dst, A64_X7, 0),
            _ => {}
        }
    }

    fn emit_ld_imm64(buf: &mut JitBuffer, insn: &BpfInsn, next_imm: i32) {
        let dst = bpf_to_a64(insn.dst_reg());
        let imm_lo = insn.imm as u64;
        let imm_hi = next_imm as u64;
        let val = (imm_hi << 32) | (imm_lo & 0xffffffff);
        let start = buf.offset();
        emit_load_imm64(buf, dst, val);
        let emitted = buf.offset() - start;
        // Pad to 24 bytes (6 instructions)
        let pad = if emitted < 24 { (24 - emitted) / 4 } else { 0 };
        for _ in 0..pad {
            a64_nop(buf);
        }
    }

    fn emit_call(buf: &mut JitBuffer, helper_fn: HelperFn) {
        // Save BPF R5 (x5) before rearranging args
        a64_mov(buf, A64_X6, A64_X5);
        // Rearrange: BPF R1-R5 → helper args (x0-x4)
        // x0 = BPF R1 (x1), x1 = BPF R2 (x2), x2 = BPF R3 (x3), x3 = BPF R4 (x4), x4 = BPF R5 (x5)
        a64_mov(buf, A64_X0, A64_X1);
        a64_mov(buf, A64_X1, A64_X2);
        a64_mov(buf, A64_X2, A64_X3);
        a64_mov(buf, A64_X3, A64_X4);
        a64_mov(buf, A64_X4, A64_X6);
        // Load helper fn address and call
        emit_load_imm64_padded(buf, A64_X16, helper_fn as u64);
        a64_blr(buf, A64_X16);
    }
}

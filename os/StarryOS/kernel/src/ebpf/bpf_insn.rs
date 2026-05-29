#![allow(dead_code)]
pub const BPF_LD: u8 = 0x00;
pub const BPF_LDX: u8 = 0x01;
pub const BPF_ST: u8 = 0x02;
pub const BPF_STX: u8 = 0x03;
pub const BPF_ALU: u8 = 0x04;
pub const BPF_JMP: u8 = 0x05;
pub const BPF_JMP32: u8 = 0x06;
pub const BPF_ALU64: u8 = 0x07;

pub const BPF_W: u8 = 0x00;
pub const BPF_H: u8 = 0x08;
pub const BPF_B: u8 = 0x10;
pub const BPF_DW: u8 = 0x18;

pub const BPF_IMM: u8 = 0x00;
pub const BPF_ABS: u8 = 0x20;
pub const BPF_IND: u8 = 0x40;
pub const BPF_MEM: u8 = 0x60;
pub const BPF_LEN: u8 = 0x80;
pub const BPF_MSH: u8 = 0xa0;

pub const BPF_ADD: u8 = 0x00;
pub const BPF_SUB: u8 = 0x10;
pub const BPF_MUL: u8 = 0x20;
pub const BPF_DIV: u8 = 0x30;
pub const BPF_OR: u8 = 0x40;
pub const BPF_AND: u8 = 0x50;
pub const BPF_LSH: u8 = 0x60;
pub const BPF_RSH: u8 = 0x70;
pub const BPF_NEG: u8 = 0x80;
pub const BPF_MOD: u8 = 0x90;
pub const BPF_XOR: u8 = 0xa0;
pub const BPF_MOV: u8 = 0xb0;
pub const BPF_ARSH: u8 = 0xc0;
pub const BPF_END: u8 = 0xd0;

pub const BPF_JA: u8 = 0x00;
pub const BPF_EXIT: u8 = 0x90;
pub const BPF_JEQ: u8 = 0x10;
pub const BPF_JGT: u8 = 0x20;
pub const BPF_JGE: u8 = 0x30;
pub const BPF_JSET: u8 = 0x40;
pub const BPF_JNE: u8 = 0x50;
pub const BPF_JSGT: u8 = 0x60;
pub const BPF_JSGE: u8 = 0x70;
pub const BPF_JLT: u8 = 0xa0;
pub const BPF_JLE: u8 = 0xb0;
pub const BPF_JSLT: u8 = 0xc0;
pub const BPF_JSLE: u8 = 0xd0;

pub const BPF_K: u8 = 0x00;
pub const BPF_X: u8 = 0x08;

pub const BPF_PSEUDO_MAP_FD: u8 = 1;
pub const BPF_PSEUDO_MAP_VALUE: u8 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BpfInsn {
    pub code: u8,
    pub dst_src_reg: u8,
    pub off: i16,
    pub imm: i32,
}

impl BpfInsn {
    pub const fn new(code: u8, dst: u8, src: u8, off: i16, imm: i32) -> Self {
        Self {
            code,
            dst_src_reg: (dst & 0xf) | ((src & 0xf) << 4),
            off,
            imm,
        }
    }

    pub fn dst_reg(&self) -> u8 {
        self.dst_src_reg & 0xf
    }

    pub fn src_reg(&self) -> u8 {
        (self.dst_src_reg >> 4) & 0xf
    }

    pub fn class(&self) -> u8 {
        self.code & 0x07
    }

    pub fn size(&self) -> u8 {
        self.code & 0x18
    }

    pub fn mode(&self) -> u8 {
        self.code & 0xe0
    }

    pub fn alu_op(&self) -> u8 {
        self.code & 0xf0
    }

    pub fn is_ld_dw_imm(&self) -> bool {
        self.code == (BPF_LD | BPF_IMM | BPF_DW)
    }

    pub fn to_bytes(self) -> [u8; 8] {
        let mut buf = [0u8; 8];
        buf[0] = self.code;
        buf[1] = self.dst_src_reg;
        buf[2..4].copy_from_slice(&self.off.to_le_bytes());
        buf[4..8].copy_from_slice(&self.imm.to_le_bytes());
        buf
    }

    pub fn from_bytes(bytes: &[u8; 8]) -> Self {
        Self {
            code: bytes[0],
            dst_src_reg: bytes[1],
            off: i16::from_le_bytes([bytes[2], bytes[3]]),
            imm: i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        }
    }
}

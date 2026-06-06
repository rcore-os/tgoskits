pub(crate) const BPF_LD: u8 = 0x00;
pub(crate) const BPF_LDX: u8 = 0x01;
pub(crate) const BPF_ST: u8 = 0x02;
pub(crate) const BPF_STX: u8 = 0x03;
pub(crate) const BPF_ALU: u8 = 0x04;
pub(crate) const BPF_JMP: u8 = 0x05;
pub(crate) const BPF_JMP32: u8 = 0x06;
pub(crate) const BPF_ALU64: u8 = 0x07;

pub(crate) const BPF_W: u8 = 0x00;
pub(crate) const BPF_H: u8 = 0x08;
pub(crate) const BPF_B: u8 = 0x10;
pub(crate) const BPF_DW: u8 = 0x18;

pub(crate) const BPF_IMM: u8 = 0x00;
pub(crate) const BPF_MEM: u8 = 0x60;

pub(crate) const BPF_ADD: u8 = 0x00;
pub(crate) const BPF_SUB: u8 = 0x10;
pub(crate) const BPF_MUL: u8 = 0x20;
pub(crate) const BPF_DIV: u8 = 0x30;
pub(crate) const BPF_OR: u8 = 0x40;
pub(crate) const BPF_AND: u8 = 0x50;
pub(crate) const BPF_LSH: u8 = 0x60;
pub(crate) const BPF_RSH: u8 = 0x70;
pub(crate) const BPF_NEG: u8 = 0x80;
pub(crate) const BPF_MOD: u8 = 0x90;
pub(crate) const BPF_XOR: u8 = 0xa0;
pub(crate) const BPF_MOV: u8 = 0xb0;
pub(crate) const BPF_ARSH: u8 = 0xc0;
pub(crate) const BPF_END: u8 = 0xd0;

pub(crate) const BPF_JA: u8 = 0x00;
pub(crate) const BPF_EXIT: u8 = 0x90;
pub(crate) const BPF_JEQ: u8 = 0x10;
pub(crate) const BPF_JGT: u8 = 0x20;
pub(crate) const BPF_JGE: u8 = 0x30;
pub(crate) const BPF_JSET: u8 = 0x40;
pub(crate) const BPF_JNE: u8 = 0x50;
pub(crate) const BPF_JSGT: u8 = 0x60;
pub(crate) const BPF_JSGE: u8 = 0x70;
pub(crate) const BPF_JLT: u8 = 0xa0;
pub(crate) const BPF_JLE: u8 = 0xb0;
pub(crate) const BPF_JSLT: u8 = 0xc0;
pub(crate) const BPF_JSLE: u8 = 0xd0;

pub(crate) const BPF_X: u8 = 0x08;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BpfInsn {
    pub code: u8,
    pub dst_src_reg: u8,
    pub off: i16,
    pub imm: i32,
}

impl BpfInsn {
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
}

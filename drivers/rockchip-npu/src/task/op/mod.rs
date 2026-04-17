use crate::op::matmul::MatMul;

pub mod matmul;

#[allow(unused)]
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum Precision {
    Int8 = 0x0,
    Float16 = 0x2,
    Int32 = 0x4,
    Float32 = 0x5,
}

pub enum Operation {
    MatMulu8(MatMul<i8, i32>),
}

impl Operation {
    pub fn reg_amount(&self) -> u32 {
        112
    }

    pub fn fill_regcmd(&self, regcmd: &mut [u64]) {
        match self {
            Operation::MatMulu8(op) => {
                op.fill_regcmd(regcmd);
            }
        }
    }
}

pub trait OperationTrait {
    fn fill_regcmd(&self, regcmd: &mut [u64]);
}

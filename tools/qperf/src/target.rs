use std::{mem::size_of, str::FromStr};

use anyhow::bail;
use zerocopy::{FromBytes, IntoBytes};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Riscv64,
    LoongArch64,
    X86_64,
}

impl FromStr for Target {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "riscv64" => Ok(Self::Riscv64),
            "loongarch64" => Ok(Self::LoongArch64),
            "x86_64" => Ok(Self::X86_64),
            _ => bail!("unknown target: {name}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg {
    Sp,
    Fp,
}

#[derive(Debug, Default, Clone, Copy, FromBytes, IntoBytes)]
#[repr(C)]
pub struct Frame {
    pub fp: u64,
    pub ip: u64,
}

impl Target {
    pub fn reg(self, reg: Reg) -> &'static str {
        match self {
            Self::Riscv64 => match reg {
                Reg::Sp => "sp",
                Reg::Fp => "fp",
            },
            Self::LoongArch64 => match reg {
                Reg::Sp => "r3",
                Reg::Fp => "r22",
            },
            Self::X86_64 => match reg {
                Reg::Sp => "rsp",
                Reg::Fp => "rbp",
            },
        }
    }

    pub fn frame_address(self, fp: u64) -> Option<u64> {
        match self {
            Self::Riscv64 | Self::LoongArch64 => fp.checked_sub(size_of::<Frame>() as u64),
            Self::X86_64 => Some(fp),
        }
    }
}

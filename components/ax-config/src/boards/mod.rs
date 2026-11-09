//! Shared board identities and genuinely cross-OS feature fragments.

use crate::Board;

pub const TARGET_AARCH64: &str = "aarch64-unknown-none-softfloat";
pub const TARGET_RISCV64: &str = "riscv64gc-unknown-none-elf";
pub const TARGET_LOONGARCH64: &str = "loongarch64-unknown-none-softfloat";
pub const TARGET_X86_64: &str = "x86_64-unknown-none";

pub mod features;

mod aka_00_sg2002;
mod asus_nuc15crh_x86_64;
mod jl_lsgd2k10;
mod k230_canmv;
mod licheerv_nano_sg2002;
mod ls2k1000;
mod orangepi_5_plus;
mod phytiumpi;
mod qemu_aarch64;
mod qemu_loongarch64;
mod qemu_riscv64;
mod qemu_x86_64;
mod rdk_s100;
mod roc_rk3568_pc;
mod rock_4d;
mod tac_e400;
mod visionfive2;

pub use aka_00_sg2002::AKA_00_SG2002;
pub use asus_nuc15crh_x86_64::ASUS_NUC15CRH_X86_64;
pub use jl_lsgd2k10::JL_LSGD2K10;
pub use k230_canmv::K230_CANMV;
pub use licheerv_nano_sg2002::LICHEERV_NANO_SG2002;
pub use ls2k1000::LS2K1000;
pub use orangepi_5_plus::ORANGEPI_5_PLUS;
pub use phytiumpi::PHYTIUMPI;
pub use qemu_aarch64::QEMU_AARCH64;
pub use qemu_loongarch64::QEMU_LOONGARCH64;
pub use qemu_riscv64::QEMU_RISCV64;
pub use qemu_x86_64::QEMU_X86_64;
pub use rdk_s100::RDK_S100;
pub use roc_rk3568_pc::ROC_RK3568_PC;
pub use rock_4d::ROCK_4D;
pub use tac_e400::TAC_E400;
pub use visionfive2::VISIONFIVE2;

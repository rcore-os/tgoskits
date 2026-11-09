//! Shared board identities and genuinely cross-OS feature fragments.

pub const TARGET_AARCH64: &str = "aarch64-unknown-none-softfloat";
pub const TARGET_RISCV64: &str = "riscv64gc-unknown-none-elf";
pub const TARGET_LOONGARCH64: &str = "loongarch64-unknown-none-softfloat";
pub const TARGET_X86_64: &str = "x86_64-unknown-none";

mod loongarch;
mod other;
mod phytium;
mod qemu;
pub mod rockchip;
mod sg2002;

pub use loongarch::{JL_LSGD2K10, LS2K1000};
pub use other::{ASUS_NUC15CRH_X86_64, K230_CANMV, RDK_S100, TAC_E400, VISIONFIVE2};
pub use phytium::PHYTIUMPI;
pub use qemu::{QEMU_AARCH64, QEMU_LOONGARCH64, QEMU_RISCV64, QEMU_X86_64};
pub use rockchip::{ORANGEPI_5_PLUS, ROC_RK3568_PC, ROCK_4D};
pub use sg2002::{AKA_00_SG2002, LICHEERV_NANO_SG2002};

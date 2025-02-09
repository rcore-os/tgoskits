#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
mod riscv;
#[cfg(target_arch = "loongarch64")]
mod loongarch64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub use riscv::*;
#[cfg(target_arch = "loongarch64")]
pub use loongarch64::*;

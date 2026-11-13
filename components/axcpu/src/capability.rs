//! CPU hardware identity.

#[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
pub use crate::arch::current::capability::has_hypervisor_extension;
#[cfg(target_arch = "loongarch64")]
pub use crate::arch::current::capability::read_cpucfg;
#[cfg(all(target_arch = "riscv64", feature = "virtualization"))]
pub use crate::arch::current::capability::vector_register_bytes;
#[cfg(target_arch = "x86_64")]
pub use crate::arch::current::capability::{
    CpuId, CpuidResult, Hypervisor, cpuid, physical_address_bits,
};
#[cfg(target_arch = "aarch64")]
pub use crate::arch::current::capability::{
    IdRegister, Midr, physical_address_bits, read_midr_el1,
};

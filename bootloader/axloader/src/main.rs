#![cfg_attr(
    any(
        target_os = "uefi",
        all(target_arch = "loongarch64", feature = "arceos-efi")
    ),
    no_std
)]
#![cfg_attr(target_os = "uefi", no_main)]

#[cfg(any(
    target_os = "uefi",
    all(target_arch = "loongarch64", feature = "arceos-efi")
))]
extern crate alloc;

#[cfg(all(target_arch = "loongarch64", feature = "arceos-efi"))]
extern crate ax_std as std;

#[cfg(all(target_arch = "loongarch64", feature = "arceos-efi"))]
mod axstd_loader;

#[cfg(all(target_arch = "loongarch64", feature = "arceos-efi"))]
fn main() {
    axstd_loader::run();
}

#[cfg(all(
    not(any(windows, unix)),
    not(target_os = "uefi"),
    not(all(target_arch = "loongarch64", feature = "arceos-efi"))
))]
compile_error!(
    "axloader supports x86_64/RISC-V UEFI or LoongArch arceos-efi; select a supported backend"
);

#[cfg(all(
    target_os = "uefi",
    not(any(target_arch = "x86_64", target_arch = "riscv64"))
))]
compile_error!("axloader UEFI backend is only implemented for x86_64 and riscv64");

#[cfg(all(
    any(windows, unix),
    not(target_os = "uefi"),
    not(all(target_arch = "loongarch64", feature = "arceos-efi"))
))]
fn main() {}

#[cfg(target_os = "uefi")]
mod loader;

#[cfg(target_os = "uefi")]
mod uefi_runtime;

#[cfg(all(target_os = "uefi", target_arch = "riscv64"))]
mod riscv64_uefi;

#[cfg(target_os = "uefi")]
pub use loader::{console, elf_loader};

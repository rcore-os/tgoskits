#[cfg(any(target_arch = "x86_64", doc, docsrs))]
#[cfg_attr(doc, doc(cfg(target_arch = "x86_64")))]
pub mod x86_64;

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64", doc, docsrs))]
#[cfg_attr(doc, doc(cfg(any(target_arch = "riscv32", target_arch = "riscv64"))))]
pub mod riscv;

#[cfg(any(target_arch = "aarch64", doc, docsrs))]
#[cfg_attr(doc, doc(cfg(target_arch = "aarch64")))]
pub mod aarch64;

#[cfg(any(target_arch = "arm", doc, docsrs))]
#[cfg_attr(doc, doc(cfg(target_arch = "arm")))]
pub mod arm;

#[cfg(any(target_arch = "loongarch64", doc, docsrs))]
#[cfg_attr(doc, doc(cfg(target_arch = "loongarch64")))]
pub mod loongarch64;

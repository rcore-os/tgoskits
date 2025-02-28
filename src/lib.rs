#![no_std]
#![feature(linkage)]
#![allow(unused)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

extern crate alloc;

#[cfg(feature = "debug")]
#[macro_use]
extern crate log;

#[cfg(not(feature = "debug"))]
#[macro_use]
mod log {
    macro_rules! trace {
        ($($arg:expr),*) => { $( let _ = $arg; )* };
    }
    macro_rules! debug {
        ($($arg:expr),*) => { $( let _ = $arg; )* };
    }
    macro_rules! info {
        ($($arg:expr),*) => { $( let _ = $arg; )*};
    }
    macro_rules! warn {
        ($($arg:expr),*) => { $( let _ = $arg; )*};
    }
    macro_rules! error {
        ($($arg:expr),*) => { $( let _ = $arg; )* };
    }
}

//mod mii_const;
mod fxmac_const;

mod utils;
mod fxmac_phy;
mod fxmac_dma;
mod fxmac_intr;
mod fxmac;

pub use fxmac::*;
pub use fxmac_dma::*;
pub use fxmac_intr::{FXmacIntrHandler, xmac_intr_handler};

// PHY interface
pub use fxmac_phy::{FXmacPhyInit, FXmacPhyRead, FXmacPhyWrite};

/// 声明网卡驱动所需的内核功能接口
#[crate_interface::def_interface]
pub trait KernelFunc{
    /// 虚拟地址转换成物理地址
    fn virt_to_phys(addr: usize) -> usize;

    /// 物理地址转换成虚拟地址
    fn phys_to_virt(addr: usize) -> usize;

    /// 申请DMA连续内存页
    fn dma_alloc_coherent(pages: usize) -> (usize, usize);

    /// 释放DMA内存页
    fn dma_free_coherent(vaddr: usize, pages: usize);

    /// 请求分配irq
    fn dma_request_irq(irq: usize, handler: fn());
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
    }
}

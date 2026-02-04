#![cfg_attr(target_os = "none", no_std)]

use core::{fmt::Display, ops::Deref, ptr::NonNull, sync::atomic::Ordering};

pub use anyhow::Error;

pub trait MmioOp: Sync + Send + 'static {
    fn ioremap(&self, addr: MmioAddr, size: usize) -> Result<Mmio, Error>;
    fn iounmap(&self, mmio: &Mmio);
}

static mut MMIO_OP: Option<&'static dyn MmioOp> = None;
static INIT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn init(mmio_op: &'static dyn MmioOp) {
    if INIT
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    unsafe {
        MMIO_OP = Some(mmio_op);
    }
}

/// # Safety
///
/// Caller should manually unmap the returned `Mmio` by calling `iounmap` when it is no longer needed.
pub unsafe fn ioremap(addr: MmioAddr, size: usize) -> Result<Mmio, Error> {
    let mmio_op = unsafe { MMIO_OP.expect("MmioOp is not initialized") };
    mmio_op.ioremap(addr, size)
}

/// # Safety
///
/// Caller must ensure that `mmio` was previously mapped by `ioremap`.
pub unsafe fn iounmap(mmio: &Mmio) {
    let mmio_op = unsafe { MMIO_OP.expect("MmioOp is not initialized") };
    mmio_op.iounmap(mmio);
}

pub fn ioremap_guard(addr: MmioAddr, size: usize) -> Result<MmioGuard, Error> {
    let mmio = unsafe { ioremap(addr, size)? };
    Ok(MmioGuard(mmio))
}

/// Physical MMIO Address
#[derive(
    Default,
    derive_more::From,
    derive_more::Into,
    Clone,
    Copy,
    derive_more::Debug,
    derive_more::Display,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
)]
#[repr(transparent)]
#[debug("PhysAddr({_0:#x})")]
#[display("{_0:#x}")]
pub struct MmioAddr(usize);

impl MmioAddr {
    pub fn as_usize(&self) -> usize {
        self.0
    }
}

impl From<u64> for MmioAddr {
    fn from(value: u64) -> Self {
        MmioAddr(value as usize)
    }
}

#[derive(Debug, Clone)]
pub struct Mmio {
    phys: MmioAddr,
    virt: NonNull<u8>,
    size: usize,
}

impl Mmio {
    /// # Safety
    ///
    /// Caller must ensure that `virt` is a valid mapping for the given `phys` and `size`.
    pub unsafe fn new(phys: MmioAddr, virt: NonNull<u8>, size: usize) -> Self {
        Mmio { phys, virt, size }
    }

    pub fn phys_addr(&self) -> MmioAddr {
        self.phys
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.virt.as_ptr(), self.size) }
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.virt.as_ptr()
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn read<T>(&self, offset: usize) -> T {
        assert!(offset < self.size);
        unsafe { self.virt.add(offset).cast::<T>().read_volatile() }
    }

    pub fn write<T>(&self, offset: usize, value: T) {
        assert!(offset < self.size);
        unsafe { self.virt.add(offset).cast::<T>().write_volatile(value) }
    }
}

pub struct MmioGuard(Mmio);

impl Deref for MmioGuard {
    type Target = Mmio;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for MmioGuard {
    fn drop(&mut self) {
        let mmio_op = unsafe { MMIO_OP.expect("MmioOp is not initialized") };
        mmio_op.iounmap(self);
    }
}

unsafe impl Send for Mmio {}
unsafe impl Sync for Mmio {}

impl Display for Mmio {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Mmio [{}, {:#x}) -> virt: {:#p}",
            self.phys,
            self.phys.0 + self.size,
            self.virt
        )
    }
}

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    use super::Mmio;

    struct DummyMmioOp;
    impl super::MmioOp for DummyMmioOp {
        fn ioremap(&self, addr: super::PhysAddr, size: usize) -> Option<Mmio> {
            Some(Mmio {
                phys: addr,
                virt: core::ptr::NonNull::dangling(),
                size,
            })
        }

        fn iounmap(&self, _mmio: &Mmio) {}
    }

    #[test]
    fn test_mmio_new() {
        super::init(&DummyMmioOp);

        let addr = Mmio {
            phys: super::PhysAddr(0x1000),
            virt: core::ptr::NonNull::dangling(),
            size: 0x100,
        };
        println!("Mmio address: {:?}", addr);
        println!("Mmio address display: {}", addr);
    }
}

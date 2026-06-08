#![cfg_attr(target_os = "none", no_std)]

use core::{fmt::Display, ops::Deref, ptr::NonNull, sync::atomic::Ordering};

#[derive(Debug)]
pub enum MapError {
    Invalid,
    NoMemory,
    Busy,
}

impl Display for MapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Invalid => write!(f, "Invalid MMIO address or size"),
            Self::NoMemory => write!(f, "Failed to allocate memory for MMIO mapping"),
            Self::Busy => write!(f, "MMIO address is already in use"),
        }
    }
}

impl core::error::Error for MapError {}

pub trait MmioOp: Sync + Send + 'static {
    fn ioremap(&self, addr: MmioAddr, size: usize) -> Result<MmioRaw, MapError>;
    fn iounmap(&self, mmio: &MmioRaw);
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
pub unsafe fn ioremap_raw(addr: MmioAddr, size: usize) -> Result<MmioRaw, MapError> {
    let mmio_op = unsafe { MMIO_OP.expect("MmioOp is not initialized") };
    mmio_op.ioremap(addr, size)
}

/// # Safety
///
/// Caller must ensure that `mmio` was previously mapped by `ioremap`.
pub unsafe fn iounmap(mmio: &MmioRaw) {
    let mmio_op = unsafe { MMIO_OP.expect("MmioOp is not initialized") };
    mmio_op.iounmap(mmio);
}

pub fn ioremap(addr: MmioAddr, size: usize) -> Result<Mmio, MapError> {
    let mmio = unsafe { ioremap_raw(addr, size)? };
    Ok(Mmio(mmio))
}

/// Physical MMIO Address
#[derive(Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct MmioAddr(usize);

impl MmioAddr {
    pub fn as_usize(&self) -> usize {
        self.0
    }
}

impl From<usize> for MmioAddr {
    fn from(value: usize) -> Self {
        MmioAddr(value)
    }
}

impl From<u64> for MmioAddr {
    fn from(value: u64) -> Self {
        MmioAddr(value as usize)
    }
}

impl From<MmioAddr> for usize {
    fn from(value: MmioAddr) -> Self {
        value.0
    }
}

impl core::fmt::Debug for MmioAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PhysAddr({:#x})", self.0)
    }
}

impl Display for MmioAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

#[derive(Debug, Clone)]
pub struct MmioRaw {
    phys: MmioAddr,
    virt: NonNull<u8>,
    size: usize,
}

impl MmioRaw {
    /// # Safety
    ///
    /// Caller must ensure that `virt` is a valid mapping for the given `phys` and `size`.
    pub unsafe fn new(phys: MmioAddr, virt: NonNull<u8>, size: usize) -> Self {
        MmioRaw { phys, virt, size }
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

    pub fn as_nonnull_ptr(&self) -> NonNull<u8> {
        self.virt
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

pub struct Mmio(MmioRaw);

impl Deref for Mmio {
    type Target = MmioRaw;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for Mmio {
    fn drop(&mut self) {
        let mmio_op = unsafe { MMIO_OP.expect("MmioOp is not initialized") };
        mmio_op.iounmap(self);
    }
}

unsafe impl Send for MmioRaw {}
unsafe impl Sync for MmioRaw {}

impl Display for MmioRaw {
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
    use super::MmioRaw;

    struct DummyMmioOp;
    impl super::MmioOp for DummyMmioOp {
        fn ioremap(&self, addr: super::MmioAddr, size: usize) -> Result<MmioRaw, super::MapError> {
            Ok(MmioRaw {
                phys: addr,
                virt: core::ptr::NonNull::dangling(),
                size,
            })
        }

        fn iounmap(&self, _mmio: &MmioRaw) {}
    }

    #[test]
    fn test_mmio_new() {
        super::init(&DummyMmioOp);

        let addr = MmioRaw {
            phys: super::MmioAddr(0x1000),
            virt: core::ptr::NonNull::dangling(),
            size: 0x100,
        };
        println!("Mmio address: {:?}", addr);
        println!("Mmio address display: {}", addr);
    }
}

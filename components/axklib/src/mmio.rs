use core::ptr::NonNull;

use mmio_api::{MapError, Mmio, MmioAddr, MmioOp, MmioRaw};

use crate::AxError;

pub struct KlibMmio;

static MMIO: KlibMmio = KlibMmio;

pub fn op() -> &'static KlibMmio {
    &MMIO
}

pub fn init_global() {
    mmio_api::init(op());
}

pub fn ioremap(addr: MmioAddr, size: usize) -> Result<Mmio, MapError> {
    init_global();
    mmio_api::ioremap(addr, size)
}

pub fn ioremap_raw(addr: MmioAddr, size: usize) -> Result<MmioRaw, MapError> {
    op().ioremap(addr, size)
}

impl MmioOp for KlibMmio {
    fn ioremap(&self, addr: MmioAddr, size: usize) -> Result<MmioRaw, MapError> {
        if size == 0 {
            return Err(MapError::Invalid);
        }

        let virt = crate::klib::mem_iomap(addr.as_usize().into(), size).map_err(map_error)?;
        let virt = NonNull::new(virt.as_mut_ptr()).ok_or(MapError::Invalid)?;
        Ok(unsafe { MmioRaw::new(addr, virt, size) })
    }

    fn iounmap(&self, _mmio: &MmioRaw) {}
}

fn map_error(err: AxError) -> MapError {
    match err {
        AxError::NoMemory => MapError::NoMemory,
        AxError::AlreadyExists | AxError::ResourceBusy => MapError::Busy,
        _ => MapError::Invalid,
    }
}

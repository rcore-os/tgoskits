use core::ptr::NonNull;

pub use mmio_api::{MapError, Mmio, MmioAddr, MmioOp, MmioRaw, ioremap, ioremap_raw};
use page_table_generic::PagingError;

pub struct KernelMmioOp;

static KERNEL_MMIO_OP: KernelMmioOp = KernelMmioOp;

pub fn kernel_mmio_op() -> &'static KernelMmioOp {
    &KERNEL_MMIO_OP
}

pub fn init_mmio_api() {
    mmio_api::init(kernel_mmio_op());
}

impl MmioOp for KernelMmioOp {
    fn ioremap(&self, addr: MmioAddr, size: usize) -> Result<MmioRaw, MapError> {
        let mapped = super::ioremap(addr.as_usize().into(), size).map_err(map_paging_error)?;
        let virt = NonNull::new(mapped.raw() as *mut u8).ok_or(MapError::Invalid)?;

        Ok(unsafe { MmioRaw::new(addr, virt, size) })
    }

    fn iounmap(&self, mmio: &MmioRaw) {
        super::iounmap(mmio).expect("failed to unmap mmio region")
    }
}

fn map_paging_error(err: PagingError) -> MapError {
    match err {
        PagingError::NoMemory => MapError::NoMemory,
        PagingError::MappingConflict { .. } => MapError::Busy,
        PagingError::AlignmentError { .. }
        | PagingError::AddressOverflow { .. }
        | PagingError::InvalidSize { .. }
        | PagingError::HierarchyError { .. }
        | PagingError::InvalidRange { .. }
        | PagingError::NotMapped => MapError::Invalid,
    }
}

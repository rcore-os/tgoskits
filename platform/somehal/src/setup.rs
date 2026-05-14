pub use mmio_api::{MapError, MmioAddr, MmioOp, MmioRaw};

pub trait KernelOp: MmioOp {}

struct EmptyKernelOp;

impl KernelOp for EmptyKernelOp {}

impl MmioOp for EmptyKernelOp {
    fn ioremap(&self, _addr: MmioAddr, _size: usize) -> Result<MmioRaw, MapError> {
        unimplemented!()
    }

    fn iounmap(&self, _mmio: &MmioRaw) {
        unimplemented!()
    }
}

static mut KERNEL_OP: &'static dyn KernelOp = &EmptyKernelOp;

pub(crate) fn set_kernel_op(op: &'static dyn KernelOp) {
    mmio_api::init(op);
    unsafe {
        KERNEL_OP = op;
    }
}

// pub(crate) fn kernel() -> &'static dyn KernelOp {
//     unsafe { KERNEL_OP }
// }

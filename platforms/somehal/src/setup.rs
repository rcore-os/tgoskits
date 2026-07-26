pub use cpu_local::CpuIndex;
pub use mmio_api::{MapError, MmioAddr, MmioOp, MmioRaw};

/// Resolves a typed logical index from someboot's immutable CPU-area table.
pub fn cpu_index(cpu_index: usize) -> Result<CpuIndex, CpuBindError> {
    let raw_index = cpu_index;
    let cpu_index =
        CpuIndex::try_from(cpu_index).map_err(|_| CpuBindError::InvalidCpu { index: raw_index })?;
    crate::smp::percpu_data_ptr(cpu_index.as_usize())
        .ok_or(CpuBindError::MissingArea { cpu_index })?;
    Ok(cpu_index)
}

/// Failure to bind the CPU-owned architecture register before HAL startup.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CpuBindError {
    /// The selected kernel runtime does not provide CPU-local binding.
    #[error("the kernel runtime does not support CPU-local register binding")]
    Unsupported,
    /// The logical index cannot be represented by the stable binding value.
    #[error("logical CPU index {index} is outside the CPU-local index range")]
    InvalidCpu { index: usize },
    /// Someboot did not publish a runtime area for this logical CPU.
    #[error("someboot did not publish CPU-local area {cpu_index:?}")]
    MissingArea { cpu_index: CpuIndex },
    /// The final image did not install its frozen typed layout.
    #[error("the final image has not installed its per-CPU layout")]
    LayoutNotInstalled,
    /// The requested CPU is outside the final image's frozen layout.
    #[error("CPU {cpu_index:?} is outside per-CPU layout area count {area_count}")]
    CpuOutOfRange {
        cpu_index: CpuIndex,
        area_count: u32,
    },
    /// Address calculation failed while selecting the CPU area.
    #[error("CPU-local area address calculation overflowed")]
    AddressOverflow,
    /// The binding does not match the runtime's installed CPU-local layout.
    #[error("CPU-local binding does not match the installed runtime layout")]
    LayoutMismatch,
    /// The architecture register could not be bound to the selected area.
    #[error(transparent)]
    CpuLocal(#[from] cpu_local::CpuLocalError),
}

pub trait KernelOp: MmioOp {
    /// Installs the CPU-owned architecture anchor before any HAL lock or IRQ
    /// path can observe the secondary CPU.
    fn bind_current_cpu(&self, cpu_index: CpuIndex) -> Result<(), CpuBindError> {
        let _ = cpu_index;
        Err(CpuBindError::Unsupported)
    }

    /// Returns the current logical CPU index after the kernel has initialized
    /// its runtime per-CPU state.
    fn current_cpu_idx(&self) -> Option<usize> {
        None
    }
}

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

pub(crate) fn kernel() -> &'static dyn KernelOp {
    unsafe { KERNEL_OP }
}

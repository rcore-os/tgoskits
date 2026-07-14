pub use mmio_api::{MapError, MmioAddr, MmioOp, MmioRaw};

/// Validated value-only description of the CPU area selected by someboot.
///
/// The platform runtime must still match this value against its installed
/// CPU-local layout before installing the architecture register. Keeping the
/// fields private prevents higher layers from silently substituting another
/// CPU or address after someboot resolved the immutable metadata table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct CpuRegisterBinding {
    area_base: usize,
    cpu_index: u32,
    reserved: u32,
}

impl CpuRegisterBinding {
    /// Returns the logical CPU selected by immutable boot metadata.
    pub const fn cpu_index(self) -> u32 {
        self.cpu_index
    }

    /// Returns the mapped runtime base of that CPU's area.
    pub const fn area_base(self) -> usize {
        self.area_base
    }
}

/// Resolves a value-only binding from someboot's immutable CPU-area table.
pub fn cpu_register_binding(cpu_index: usize) -> Result<CpuRegisterBinding, CpuBindError> {
    let cpu_index = u32::try_from(cpu_index).map_err(|_| CpuBindError::InvalidCpu)?;
    let area_base =
        crate::smp::percpu_data_ptr(cpu_index as usize).ok_or(CpuBindError::MissingArea)? as usize;
    Ok(CpuRegisterBinding {
        area_base,
        cpu_index,
        reserved: 0,
    })
}

/// Failure to bind the CPU-owned architecture register before HAL startup.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CpuBindError {
    /// The selected kernel runtime does not provide CPU-local binding.
    #[error("the kernel runtime does not support CPU-local register binding")]
    Unsupported,
    /// The logical index cannot be represented by the stable binding value.
    #[error("logical CPU index is outside the CPU-local binding ABI")]
    InvalidCpu,
    /// Someboot did not publish a runtime area for this logical CPU.
    #[error("someboot did not publish the selected CPU-local area")]
    MissingArea,
    /// The binding does not match the runtime's installed CPU-local layout.
    #[error("CPU-local binding does not match the installed runtime layout")]
    LayoutMismatch,
    /// The architecture register could not be bound to the selected area.
    #[error("failed to install the CPU-local architecture register")]
    Register,
}

pub trait KernelOp: MmioOp {
    /// Installs the CPU-owned architecture anchor before any HAL lock or IRQ
    /// path can observe the secondary CPU.
    fn bind_current_cpu(&self, binding: CpuRegisterBinding) -> Result<(), CpuBindError> {
        let _ = binding;
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

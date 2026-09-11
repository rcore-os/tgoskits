use super::register::irq as cpuintc;
use crate::irq::IrqId;

/// CPU 中断源数量 (SWI0-1, HWI0-7, PCOV, TI, IPI, NMI, AVEC)
const EXCCODE_INT_NUM: usize = 15;

/// 中断类型，包含硬件中断号
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqKind {
    /// CPU 本地中断 (SWI, HWI, TI, IPI, PMC, NMI, AVEC)
    /// hwirq 范围: 0-14，对应 ESTAT.IS 位
    Private(usize),
    /// 外部中断 (通过级联中断控制器)
    /// hwirq 为级联控制器的中断号
    External(usize),
}

impl IrqKind {
    /// 获取硬件中断号
    pub fn hwirq(&self) -> usize {
        match self {
            IrqKind::Private(hwirq) => *hwirq,
            IrqKind::External(hwirq) => *hwirq,
        }
    }

    /// 检查是否为私有中断
    pub fn is_private(&self) -> bool {
        matches!(self, IrqKind::Private(_))
    }

    /// 检查是否为外部中断
    pub fn is_external(&self) -> bool {
        matches!(self, IrqKind::External(_))
    }
}

impl IrqId {
    /// 创建 CPU 私有中断号
    /// hwirq: 硬件中断号 (0-14)
    pub fn private_irq(hwirq: usize) -> Self {
        debug_assert!(hwirq < EXCCODE_INT_NUM, "hwirq {hwirq} out of range");
        Self::new(hwirq)
    }

    /// 创建外部中断号
    /// 外部中断通过级联控制器路由，软件中断号 = hwirq + CPU_INT_NUM
    pub fn extern_irq(hwirq: usize) -> Self {
        Self::new(hwirq + EXCCODE_INT_NUM)
    }

    /// 获取中断类型及硬件中断号
    pub fn kind(&self) -> IrqKind {
        let raw = self.raw();
        if raw < EXCCODE_INT_NUM {
            IrqKind::Private(raw)
        } else {
            IrqKind::External(raw - EXCCODE_INT_NUM)
        }
    }

    /// 检查是否为定时器中断
    pub fn is_timer(&self) -> bool {
        self.raw() == cpuintc::TI as usize
    }

    /// 检查是否为 IPI 中断
    pub fn is_ipi(&self) -> bool {
        self.raw() == cpuintc::IPI as usize
    }
}

pub fn per_cpu_trap_init(is_primary: bool) {
    let vector = ax_cpu::boot::boot_vector();
    let refill = crate::mem::virt_to_phys(ax_cpu::boot::tlb_refill_entry() as *const u8);
    // SAFETY: boot owns PLV0 with IRQs masked and retains vectors, stack and policy.
    unsafe { ax_cpu::boot::install_boot_vectors(vector, refill.into()) };
    if is_primary {
        println!("CPU boot vectors: {vector:#x}, refill: {refill:#x}");
    }
}

#[cfg_attr(axtest_coverage, coverage(off))]
pub(crate) fn init_entries_for_secondary() {
    let mask = (1usize << super::addrspace::PABITS) - 1;
    // SAFETY: the secondary owns its published boot stack; PC-relative CPU
    // entry addresses are valid before final image relocation and vector setup.
    unsafe {
        ax_cpu::boot::install_boot_vectors(
            ax_cpu::boot::boot_vector(),
            (ax_cpu::boot::tlb_refill_entry() & mask).into(),
        )
    };
}

struct BootTrap;
#[trait_ffi::impl_extern_trait]
impl ax_cpu::trap::boot::BootTrapHandler for BootTrap {
    fn handle(exception: &ax_cpu::trap::boot::BootException) {
        panic!("unexpected LoongArch exception before runtime setup: {exception:?}");
    }
}

use core::fmt::Debug;

use crate::{Rknpu, RknpuError, RknpuType};

/// Returns a mask with the lowest `n` bits set.
/// Mirrors the C macro: (((n) == 64) ? ~0ULL : ((1ULL<<(n))-1))
pub(crate) const fn dma_bit_mask(n: u32) -> u64 {
    if n >= 64 {
        u64::MAX
    } else {
        (1u64 << n) - 1u64
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct RknpuAmountData {
    pub offset_clr_all: u16,
    pub offset_dt_wr: u16,
    pub offset_dt_rd: u16,
    pub offset_wt_rd: u16,
}

#[derive(Debug, Clone)]
pub(crate) struct RknpuData {
    pub bw_priority_addr: u32,
    pub bw_priority_length: u32,
    pub dma_mask: u64,
    pub pc_data_amount_scale: u32,
    pub pc_task_number_bits: u32,
    pub pc_task_number_mask: u32,
    pub pc_task_status_offset: u32,
    pub pc_dma_ctrl: u32,
    pub nbuf_phyaddr: u64,
    pub nbuf_size: u64,
    pub max_submit_number: u64,
    pub core_mask: u32,
    pub irqs: &'static [NpuIrq],
    /// Pointer to top-level amount data (opaque).
    pub amount_top: Option<RknpuAmountData>,
    /// Pointer to per-core amount data (opaque).
    pub amount_core: Option<RknpuAmountData>,
    /// Platform-specific state initialization function
    pub state_init: Option<fn(&mut dyn core::any::Any) -> Result<(), RknpuError>>,
    /// Cache scatter-gather table initialization
    pub cache_sgt_init: Option<fn(&mut dyn core::any::Any) -> Result<(), RknpuError>>,
}

impl RknpuData {
    pub fn new(ty: RknpuType) -> Self {
        match ty {
            RknpuType::Rk3588 => Self::new_3588(),
        }
    }

    fn new_3588() -> Self {
        Self {
            bw_priority_addr: 0x0,
            bw_priority_length: 0x0,
            dma_mask: dma_bit_mask(40),
            pc_data_amount_scale: 2,
            pc_task_number_bits: 12,
            pc_task_number_mask: 0xfff,
            pc_task_status_offset: 0x3c,
            pc_dma_ctrl: 0,
            irqs: RK3588_IRQS,
            nbuf_phyaddr: 0,
            nbuf_size: 0,
            max_submit_number: (1u64 << 12) - 1u64,
            core_mask: 0x7,
            amount_top: None,
            amount_core: None,
            state_init: None,
            cache_sgt_init: None,
        }
    }
}

const RK3588_IRQS: &[NpuIrq] = &[
    NpuIrq {
        name: "npu0_irq",
        irq_hdl: |_, _| None,
    },
    NpuIrq {
        name: "npu1_irq",
        irq_hdl: |_, _| None,
    },
    NpuIrq {
        name: "npu2_irq",
        irq_hdl: |_, _| None,
    },
];

pub struct NpuIrq {
    pub name: &'static str,
    pub irq_hdl: fn(&mut Rknpu, irq: usize) -> Option<()>,
}

impl Debug for NpuIrq {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NpuIrq").field("name", &self.name).finish()
    }
}

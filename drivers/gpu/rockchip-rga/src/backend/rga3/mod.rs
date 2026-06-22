//! RGA3 backend. RGA3 (RK3588) is programmed differently from RGA2 (different task/register layout,
//! and the shared `rockchip,iommu-v2` for non-contiguous buffers). PR-1 ships a skeleton only.
pub mod registers;

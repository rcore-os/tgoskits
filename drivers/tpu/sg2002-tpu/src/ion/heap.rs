//! Ion 堆管理

use alloc::sync::Arc;

use dma_api::DeviceDma;

use super::{
    error::{IonError, IonResult},
    types::{IonBuffer, IonHeapType},
};

/// Ion 堆管理器
pub struct IonHeapManager {
    dma: DeviceDma,
}

impl IonHeapManager {
    /// 创建新的堆管理器
    pub fn new(dma: DeviceDma) -> Self {
        Self { dma }
    }

    /// 从指定堆分配缓冲区
    pub fn alloc_buffer(
        &self,
        size: usize,
        align: usize,
        heap_type: IonHeapType,
    ) -> IonResult<Arc<IonBuffer>> {
        debug!(
            "Allocating Ion buffer: size={}, align={}, heap_type={:?}",
            size, align, heap_type
        );
        // 校验参数
        if size == 0 {
            return Err(IonError::InvalidArg);
        }

        let dma = match heap_type {
            IonHeapType::System | IonHeapType::DmaCoherent => self
                .dma
                .coherent_array_zero_with_align(size, align)
                .map_err(|_| IonError::NoMemory)?,
            IonHeapType::Carveout => {
                return Err(IonError::NotSupported);
            }
        };

        let buffer = Arc::new(IonBuffer::new(dma, size));
        debug!("Allocated Ion buffer with handle: {:?}", buffer.handle);

        Ok(buffer)
    }
}

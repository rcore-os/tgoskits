//! Ion Buffer 文件类型
//!
//! 实现 FileLike trait，用于支持对 Ion 分配的缓冲区进行 mmap。

use alloc::{borrow::Cow, sync::Arc};

use ax_errno::{AxError, AxResult};
use ax_memory_addr::PhysAddrRange;
use axpoll::{IoEvents, Pollable};
use sg2002_tpu::ion::IonBuffer;

use super::{FileLike, Kstat};
use crate::pseudofs::{
    DeviceMmap, DeviceOps,
    dev::{
        ION_DEVICE,
        ion::{ION_IOC_FREE, IonHandleData},
    },
};

/// Ion Buffer 文件
///
/// 持有底层 [`IonBuffer`] 的强引用，保证只要 fd（以及由 fd 衍生的 mmap）还存活，
/// 物理页就不会被归还给 DMA / page allocator。
pub struct IonBufferFile {
    /// 底层缓冲区
    buffer: Arc<IonBuffer>,
}

impl IonBufferFile {
    /// 创建新的 Ion Buffer 文件
    pub fn new(buffer: Arc<IonBuffer>) -> Self {
        Self { buffer }
    }

    /// 获取物理地址范围
    pub fn phys_range(&self) -> PhysAddrRange {
        PhysAddrRange::from_start_size(
            ax_memory_addr::PhysAddr::from(self.buffer.dma_info.bus_addr.as_u64() as usize),
            self.buffer.size,
        )
    }

    /// 获取底层缓冲区的强引用
    pub fn buffer(&self) -> &Arc<IonBuffer> {
        &self.buffer
    }
}

impl Pollable for IonBufferFile {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut core::task::Context<'_>, _events: IoEvents) {
        // Ion buffer 总是就绪
    }
}

impl FileLike for IonBufferFile {
    fn read(&self, _dst: &mut super::IoDst) -> AxResult<usize> {
        // Ion buffer 不支持直接读取
        Err(AxError::InvalidInput)
    }

    fn write(&self, _src: &mut super::IoSrc) -> AxResult<usize> {
        // Ion buffer 不支持直接写入
        Err(AxError::InvalidInput)
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat {
            size: self.buffer.size as u64,
            ..Default::default()
        })
    }

    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed("/dev/ion_buffer")
    }

    fn device_mmap(&self, _offset: u64) -> AxResult<DeviceMmap> {
        Ok(DeviceMmap::Physical(self.phys_range()))
    }
}

impl Drop for IonBufferFile {
    fn drop(&mut self) {
        let handle = self.buffer.handle.as_u32();
        debug!("Dropping IonBufferFile, releasing handle: {}", handle);
        // fd 关闭时，向全局 ion 设备发起一次 FREE，确保用户未显式调用
        // ION_IOC_FREE 的情况下，全局 buffer 表里的强引用也会被移除。
        // 物理页的真正释放由最后一个 `Arc<IonBuffer>` 在 Drop 时完成。
        if let Some(dev) = ION_DEVICE.get() {
            let handle_data = IonHandleData { handle };
            let _ = dev.ioctl(ION_IOC_FREE, &handle_data as *const _ as usize);
        } else {
            error!(
                "Failed to find ion device to free buffer handle: {}",
                handle
            );
        }
    }
}

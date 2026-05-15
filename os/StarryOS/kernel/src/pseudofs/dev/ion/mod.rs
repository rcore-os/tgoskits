//! Ion (Android ION) memory allocator driver — OS 胶水层
//!
//! 硬件驱动逻辑已迁移至 `sg2002-tpu` crate。本模块仅保留 StarryOS
//! 特定的全局状态（`ION_DEVICE`、`global_ion_buffer_manager`）以及
//! 设备入口 [`IonDevice`]。

mod device;

use alloc::sync::Arc;

pub use device::IonDevice;
// 从 sg2002-tpu 重新导出 OS 层实际使用的驱动类型
pub use sg2002_tpu::ion::{IonBufferManager, IonHandleData, ioctl::ION_IOC_FREE};
use spin::Once;

/// 全局共享的 Ion Buffer 管理器
static GLOBAL_ION_BUFFER_MANAGER: Once<Arc<IonBufferManager>> = Once::new();

/// 获取全局 Ion Buffer 管理器
pub fn global_ion_buffer_manager() -> Arc<IonBufferManager> {
    GLOBAL_ION_BUFFER_MANAGER
        .call_once(|| Arc::new(IonBufferManager::new()))
        .clone()
}

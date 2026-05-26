//! Ion 缓冲区管理

use alloc::{collections::BTreeMap, sync::Arc};

use ax_kspin::SpinNoIrq as Mutex;

use super::{
    error::{IonError, IonResult},
    types::{IonBuffer, IonHandle},
};

/// Ion 缓冲区管理器
pub struct IonBufferManager {
    /// 已分配的缓冲区映射
    buffers: Mutex<BTreeMap<IonHandle, Arc<IonBuffer>>>,
}

impl Default for IonBufferManager {
    fn default() -> Self {
        Self::new()
    }
}

impl IonBufferManager {
    /// 创建新的缓冲区管理器
    pub fn new() -> Self {
        Self {
            buffers: Mutex::new(BTreeMap::new()),
        }
    }

    /// 注册缓冲区
    pub fn register_buffer(&self, buffer: Arc<IonBuffer>) -> IonResult<()> {
        let mut buffers = self.buffers.lock();
        let handle = buffer.handle;

        if buffers.contains_key(&handle) {
            return Err(IonError::BufferExists);
        }

        buffers.insert(handle, buffer);
        debug!("Registered Ion buffer with handle: {:?}", handle);
        Ok(())
    }

    /// 取消注册缓冲区
    pub fn unregister_buffer(&self, handle: IonHandle) -> IonResult<Arc<IonBuffer>> {
        let mut buffers = self.buffers.lock();
        let buffer = buffers.remove(&handle).ok_or(IonError::BufferNotFound)?;

        debug!("Unregistered Ion buffer with handle: {:?}", handle);
        Ok(buffer)
    }

    /// 获取缓冲区
    pub fn get_buffer(&self, handle: IonHandle) -> IonResult<Arc<IonBuffer>> {
        let buffers = self.buffers.lock();
        buffers
            .get(&handle)
            .cloned()
            .ok_or(IonError::BufferNotFound)
    }

    /// 清理所有缓冲区
    pub fn cleanup_all(&self) {
        let mut buffers = self.buffers.lock();
        let count = buffers.len();
        buffers.clear();
        if count > 0 {
            warn!("Cleaned up {} Ion buffers", count);
        }
    }
}

//! V4L2 驱动 trait。

use alloc::{sync::Arc, vec::Vec};

use axpoll::PollSet;

use crate::{
    ctrls::CtrlHandler,
    ioctl::{IoctlOps, LegacyIoctlOps},
};

/// V4L2 驱动对象。
#[allow(unused_variables)]
pub trait V4L2DriverOps: Send + Sync + IoctlOps + LegacyIoctlOps {
    /// mmap 解析。
    fn mmap(&self, offset: u64, length: u64) -> Option<(Vec<usize>, usize)> {
        None
    }

    /// 是否可读。
    fn is_readable(&self) -> bool {
        false
    }

    /// 是否错误。
    fn is_error(&self) -> bool {
        false
    }

    /// 是否推流中。
    fn is_streaming(&self) -> bool {
        false
    }

    /// 已分配缓冲数。
    fn num_buffers(&self) -> u32 {
        0
    }

    /// 获取 vb2 唤醒源。
    fn vb_poll_set(&self) -> Option<Arc<PollSet>> {
        None
    }

    /// 释放资源。
    fn release(&self) {}

    /// 获取控件处理器。
    fn ctrl_handler(&self) -> Option<&CtrlHandler> {
        None
    }
}

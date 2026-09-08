//! V4L2 视频设备 — 将 v4l2 与 StarryOS pseudofs 的 DeviceOps 桥接起来。
//!
//! 这是 `/dev/videoX` 的实现。它包装了一个 `ax_media::device::VideoDevice`，
//! 并处理 ioctl ABI：从用户空间指针读取 C 结构体、
//! 分发到 V4L2 ioctl 引擎，再写回结果。

use alloc::{sync::Arc, vec, vec::Vec};
use core::{any::Any, mem::MaybeUninit, task::Context};

use ax_media::{
    IoctlCmd, V4l2Error, VideoDevice, VideoIoctl,
    interface::{
        ctrl::{ExtControl, ExtControls},
        event::Event,
    },
};
use ax_memory_addr::PhysAddr;
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use axpoll::{IoEvents, PollSet, Pollable};
use starry_vm::{VmError, vm_read_slice, vm_write_slice};

use crate::{
    StarryError,
    pseudofs::{DeviceMmap, DeviceOps},
};

/// 将用户内存访问错误映射为 VFS 错误（经 StarryError 桥接）。
fn vm_to_vfs(e: VmError) -> VfsError {
    VfsError::from(StarryError::from(e))
}

/// V4L2 视频设备节点 — 将 `VideoDevice` 包装为 `DeviceOps`。
pub struct V4l2DevNode {
    inner: Arc<VideoDevice>,
    event_source: Option<Arc<ax_sync::Mutex<Vec<Event>>>>,
    poll_rx: Option<Arc<PollSet>>,
    event_poll_rx: Arc<PollSet>,
}

impl V4l2DevNode {
    fn new(device: VideoDevice, event_source: Option<Arc<ax_sync::Mutex<Vec<Event>>>>) -> Self {
        // 完成唤醒由驱动（vb2 队列）内建：构造时取一次 vb_poll_set，
        // 之后 register 无需设备锁。事件唤醒源同理（设备构造时内建）。
        let inner = Arc::new(device);
        let poll_rx = inner.vb_poll_set();
        let event_poll_rx = inner.event_poll_set();
        Self {
            inner,
            event_source,
            poll_rx,
            event_poll_rx,
        }
    }

    /// 从输入（采集）设备创建设备节点。
    pub fn from_input(device: VideoDevice, event_source: Arc<ax_sync::Mutex<Vec<Event>>>) -> Self {
        Self::new(device, Some(event_source))
    }

    /// 将共享驱动事件队列中的事件投递到 fh（订阅过滤在框架内完成）。
    fn drain_events(&self) {
        if let Some(ref src) = self.event_source {
            let events: Vec<Event> = core::mem::take(&mut *src.lock());
            for mut ev in events {
                self.inner.queue_event(&mut ev);
            }
        }
    }
}

impl DeviceOps for V4l2DevNode {
    fn open(&self, _exclusive: bool) -> VfsResult<()> {
        self.inner.open_fh();
        Ok(())
    }

    fn close(&self, _exclusive: bool) {
        self.inner.close_fh();
    }

    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::from(StarryError::InvalidInput))
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::from(StarryError::InvalidInput))
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        let Some(ioctl) = VideoIoctl::try_from_u32(cmd) else {
            return Err(VfsError::from(StarryError::NotATty));
        };

        // LOG_STATUS 为 _IO 无参，arg==0 合法；其余需要指针的 ioctl 在 arg==0 时应 EFAULT
        let is_log_status = matches!(ioctl, VideoIoctl::Modern(IoctlCmd::LogStatus));
        let size = if is_log_status {
            0
        } else {
            ioctl_arg_size(cmd)
        };
        if size > 0 && arg == 0 {
            return Err(VfsError::BadAddress);
        }

        // 堆分配 ioctl 缓冲：结构体大小可能超过栈安全阈值（如
        // v4l2_query_ext_ctrl ≈ 236B，ext_ctrls payload 可达 KB 级），
        // 栈上 1024B 数组在单核内核小栈下危险，且会截断大结构体。
        let mut buf_uninit: Vec<MaybeUninit<u8>> = vec![MaybeUninit::uninit(); size];
        if size > 0 && arg != 0 {
            vm_read_slice(arg as *const u8, &mut buf_uninit).map_err(vm_to_vfs)?;
        }
        // MaybeUninit<u8> → u8：u8 无 drop，assume_init 安全。
        let mut buf: Vec<u8> = buf_uninit
            .into_iter()
            .map(|v| unsafe { v.assume_init() })
            .collect();

        // ── 扩展控件：需要从用户空间读取 payload ──
        if let VideoIoctl::Modern(ioctl_cmd) = ioctl {
            match ioctl_cmd {
                IoctlCmd::GExtCtrls | IoctlCmd::SExtCtrls | IoctlCmd::TryExtCtrls => {
                    // 从已复制的参数缓冲中读取头
                    let mut header: ExtControls =
                        unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const ExtControls) };
                    let ec_count = header.count as usize;
                    let ec_size = core::mem::size_of::<ExtControl>();
                    let payload_size = ec_count * ec_size;
                    if payload_size > 0 {
                        // 从用户空间将控件数组读入堆缓冲。
                        let mut payload_uninit: Vec<MaybeUninit<u8>> =
                            vec![MaybeUninit::uninit(); payload_size];
                        vm_read_slice(header.controls as *const u8, &mut payload_uninit)
                            .map_err(vm_to_vfs)?;
                        let mut payload: Vec<u8> = payload_uninit
                            .into_iter()
                            .map(|v| unsafe { v.assume_init() })
                            .collect();

                        let result = match ioctl_cmd {
                            IoctlCmd::GExtCtrls => {
                                self.inner.handle_g_ext_ctrls(&mut header, &mut payload)
                            }
                            IoctlCmd::SExtCtrls => {
                                self.inner.handle_s_ext_ctrls(&mut header, &mut payload)
                            }
                            IoctlCmd::TryExtCtrls => {
                                self.inner.handle_try_ext_ctrls(&mut header, &mut payload)
                            }
                            _ => unreachable!(),
                        };

                        match result {
                            Ok(()) => {
                                // G/S/TRY 均回写控件数组（值可能被取整 / clamp）。
                                vm_write_slice(header.controls as *mut u8, &payload)
                                    .map_err(vm_to_vfs)?;
                                // 同时回写头（error_idx / which 可能被设置）
                                let header_bytes = unsafe {
                                    core::slice::from_raw_parts(
                                        &header as *const ExtControls as *const u8,
                                        core::mem::size_of::<ExtControls>(),
                                    )
                                };
                                vm_write_slice(arg as *mut u8, header_bytes).map_err(vm_to_vfs)?;
                                self.drain_events();
                                return Ok(0);
                            }
                            Err(err) => {
                                // 即使失败也需回写 header 的 error_idx，对齐 Linux 语义
                                let header_bytes = unsafe {
                                    core::slice::from_raw_parts(
                                        &header as *const ExtControls as *const u8,
                                        core::mem::size_of::<ExtControls>(),
                                    )
                                };
                                let _ = vm_write_slice(arg as *mut u8, header_bytes);
                                let _ = vm_write_slice(header.controls as *mut u8, &payload);
                                return Err(VfsError::from(map_v4l2_error(err)));
                            }
                        }
                    } else {
                        // count==0：无 payload，直接校验 which（类探测）
                        let mut empty_payload = Vec::new();
                        let result = match ioctl_cmd {
                            IoctlCmd::GExtCtrls => self
                                .inner
                                .handle_g_ext_ctrls(&mut header, &mut empty_payload),
                            IoctlCmd::SExtCtrls => self
                                .inner
                                .handle_s_ext_ctrls(&mut header, &mut empty_payload),
                            IoctlCmd::TryExtCtrls => self
                                .inner
                                .handle_try_ext_ctrls(&mut header, &mut empty_payload),
                            _ => unreachable!(),
                        };
                        match result {
                            Ok(()) => {
                                let header_bytes = unsafe {
                                    core::slice::from_raw_parts(
                                        &header as *const ExtControls as *const u8,
                                        core::mem::size_of::<ExtControls>(),
                                    )
                                };
                                vm_write_slice(arg as *mut u8, header_bytes).map_err(vm_to_vfs)?;
                                self.drain_events();
                                return Ok(0);
                            }
                            Err(err) => {
                                let header_bytes = unsafe {
                                    core::slice::from_raw_parts(
                                        &header as *const ExtControls as *const u8,
                                        core::mem::size_of::<ExtControls>(),
                                    )
                                };
                                let _ = vm_write_slice(arg as *mut u8, header_bytes);
                                return Err(VfsError::from(map_v4l2_error(err)));
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        match self.inner.handle_ioctl(ioctl, &mut buf[..size]) {
            Ok(()) => {
                if size > 0 && arg != 0 {
                    vm_write_slice(arg as *mut u8, &buf[..size]).map_err(vm_to_vfs)?;
                }
                self.drain_events();
                Ok(0)
            }
            Err(err) => Err(VfsError::from(map_v4l2_error(err))),
        }
    }

    fn mmap(&self, offset: u64, length: u64) -> DeviceMmap {
        if let Some((pages, _size)) = self.inner.mmap(offset, length) {
            DeviceMmap::PhysicalPages(pages.into_iter().map(PhysAddr::from_usize).collect(), None)
        } else {
            DeviceMmap::None
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

impl Pollable for V4l2DevNode {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        if self.inner.is_readable() {
            events |= IoEvents::IN;
        }
        if self.inner.is_error() {
            events |= IoEvents::ERR;
        }
        if self.inner.has_pending_events() {
            events |= IoEvents::PRI;
        }
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        let Some(poll_rx) = &self.poll_rx else {
            context.waker().wake_by_ref();
            return;
        };
        let interests = events & (IoEvents::IN | IoEvents::ERR);
        if !interests.is_empty() {
            unsafe { poll_rx.register(context.waker(), interests) }
        }
        if !(events & IoEvents::PRI).is_empty() {
            unsafe { self.event_poll_rx.register(context.waker(), IoEvents::PRI) }
        }
    }
}

/// 估算给定 ioctl 命令对应的 C 结构体大小。
fn ioctl_arg_size(cmd: u32) -> usize {
    let encoded = ((cmd >> 16) & 0x3FFF) as usize;
    if encoded == 0 { 256 } else { encoded }
}

/// 将 V4L2 驱动错误映射为 [`StarryError`]。
fn map_v4l2_error(err: V4l2Error) -> StarryError {
    match err {
        V4l2Error::InvalidArgument => StarryError::InvalidInput,
        V4l2Error::NoSuchDevice => StarryError::NoSuchDevice,
        V4l2Error::Io => StarryError::Io,
        V4l2Error::NotSupported => StarryError::NotATty,
        V4l2Error::Busy => StarryError::ResourceBusy,
        V4l2Error::Timeout => StarryError::TimedOut,
        V4l2Error::NoMemory => StarryError::NoMemory,
        V4l2Error::AccessDenied => StarryError::PermissionDenied,
        V4l2Error::BadFileDescriptor => StarryError::BadFileDescriptor,
        V4l2Error::WouldBlock => StarryError::WouldBlock,
        V4l2Error::NoEntry => StarryError::NotFound,
        V4l2Error::NoSuchDeviceOrAddress => StarryError::NoSuchDeviceOrAddress,
        V4l2Error::OperationNotPermitted => StarryError::OperationNotPermitted,
        V4l2Error::Interrupted => StarryError::Interrupted,
        V4l2Error::NotATty => StarryError::NotATty,
        V4l2Error::StorageFull => StarryError::StorageFull,
        V4l2Error::OutOfRange => StarryError::OutOfRange,
        V4l2Error::MessageTooLong => StarryError::Io,
    }
}

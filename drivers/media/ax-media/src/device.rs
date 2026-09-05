//! V4L2 设备抽象。

use alloc::{sync::Arc, vec::Vec};
use core::{
    future::poll_fn,
    sync::atomic::{AtomicU32, Ordering},
    task::Poll,
};

use ax_sync::Mutex;
use axpoll::{IoEvents, PollSet};

use crate::{
    Result, V4l2Error,
    ctrls::CtrlHandler,
    driver::V4L2DriverOps,
    filehandler::{QueueOutcome, V4l2Fh},
    interface::{
        ctrl::{ExtControl, ExtControls},
        event::{Event, EventSubscription},
    },
    ioctl::{IoctlCmd, IoctlDispatcher, VideoIoctl},
};

/// V4L2 视频设备。
pub struct VideoDevice {
    driver: Arc<Mutex<dyn V4L2DriverOps>>,
    dispatcher: Mutex<IoctlDispatcher>,
    name: &'static str,
    fh: Mutex<Option<V4l2Fh>>,
    open_count: AtomicU32,
    event_poll_rx: Arc<PollSet>,
}

impl VideoDevice {
    /// 创建设备。
    pub fn new(driver: Arc<Mutex<dyn V4L2DriverOps>>, name: &'static str) -> Self {
        Self {
            driver,
            dispatcher: Mutex::new(IoctlDispatcher::new()),
            name,
            fh: Mutex::new(None),
            open_count: AtomicU32::new(0),
            event_poll_rx: Arc::new(PollSet::new()),
        }
    }

    /// 获取设备名。
    pub fn name(&self) -> &str {
        self.name
    }

    /// 处理 ioctl。
    pub fn handle_ioctl(&self, cmd: VideoIoctl, arg: &mut [u8]) -> Result<()> {
        if let VideoIoctl::Modern(c) = cmd {
            match c {
                IoctlCmd::SubscribeEvent => {
                    let sub: EventSubscription = unsafe { crate::ioctl::read_from_bytes(arg) };
                    let mut driver = self.driver.lock();
                    let mut fh_guard = self.fh.lock();
                    let fh = fh_guard.as_mut().ok_or(V4l2Error::BadFileDescriptor)?;
                    driver.subscribe_event(fh, &sub)?;
                    if fh.pending() > 0 {
                        self.event_poll_rx.wake_from_irq(IoEvents::PRI);
                    }
                    return Ok(());
                }
                IoctlCmd::UnsubscribeEvent => {
                    let sub: EventSubscription = unsafe { crate::ioctl::read_from_bytes(arg) };
                    let mut driver = self.driver.lock();
                    let mut fh_guard = self.fh.lock();
                    let fh = fh_guard.as_mut().ok_or(V4l2Error::BadFileDescriptor)?;
                    driver.unsubscribe_event(fh, &sub)?;
                    return Ok(());
                }
                IoctlCmd::DQEvent => {
                    let mut ev: Event = unsafe { crate::ioctl::read_from_bytes(arg) };
                    let mut driver = self.driver.lock();
                    let mut fh_guard = self.fh.lock();
                    let fh = fh_guard.as_mut().ok_or(V4l2Error::BadFileDescriptor)?;
                    driver.dqevent(fh, &mut ev)?;
                    unsafe { crate::ioctl::write_to_bytes(arg, &ev) };
                    return Ok(());
                }
                IoctlCmd::GPriority => {
                    let fh_guard = self.fh.lock();
                    let fh = fh_guard.as_ref().ok_or(V4l2Error::BadFileDescriptor)?;
                    let p = fh.prio();
                    unsafe { crate::ioctl::write_to_bytes(arg, &p) };
                    return Ok(());
                }
                IoctlCmd::SPriority => {
                    let p: u32 = unsafe { crate::ioctl::read_from_bytes(arg) };
                    if p > 3 {
                        return Err(crate::V4l2Error::InvalidArgument);
                    }
                    let mut fh_guard = self.fh.lock();
                    let fh = fh_guard.as_mut().ok_or(V4l2Error::BadFileDescriptor)?;
                    fh.set_prio(p);
                    return Ok(());
                }
                IoctlCmd::DQBuf => {
                    return self.handle_dqbuf_blocking(cmd, arg);
                }
                _ => {}
            }
        }
        let mut driver = self.driver.lock();
        let dispatcher = self.dispatcher.lock();
        dispatcher.dispatch(&mut *driver, cmd, arg)
    }

    /// DQBuf 阻塞等待。
    fn handle_dqbuf_blocking(&self, cmd: VideoIoctl, arg: &mut [u8]) -> Result<()> {
        loop {
            let result = {
                let mut driver = self.driver.lock();
                let dispatcher = self.dispatcher.lock();
                dispatcher.dispatch(&mut *driver, cmd, arg)
            };
            match result {
                Ok(()) => return Ok(()),
                Err(e) if e == V4l2Error::WouldBlock || e == V4l2Error::Busy => {
                    let poll_rx = match self.vb_poll_set() {
                        Some(rx) => rx,
                        None => return Err(e),
                    };
                    let wait = poll_fn(|cx| {
                        if self.is_readable() || self.is_error() || !self.is_streaming() {
                            return Poll::Ready(());
                        }
                        // SAFETY: ioctl 任务上下文；register 不持 VideoDevice 锁。
                        unsafe { poll_rx.register(cx.waker(), IoEvents::IN | IoEvents::ERR) };
                        if self.is_readable() || self.is_error() || !self.is_streaming() {
                            Poll::Ready(())
                        } else {
                            Poll::Pending
                        }
                    });
                    ax_task::future::block_on(wait);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// 禁用指定 ioctl。
    pub fn disable_ioctl(&self, cmd: u32) {
        self.dispatcher.lock().disable_cmd(cmd);
    }

    /// mmap 查询。
    pub fn mmap(&self, offset: u64, length: u64) -> Option<(Vec<usize>, usize)> {
        self.driver.lock().mmap(offset, length)
    }

    /// 是否可读。
    pub fn is_readable(&self) -> bool {
        self.driver.lock().is_readable()
    }

    /// 是否错误。
    pub fn is_error(&self) -> bool {
        self.driver.lock().is_error()
    }

    /// 是否推流中。
    pub fn is_streaming(&self) -> bool {
        self.driver.lock().is_streaming()
    }

    /// 获取 vb2 唤醒源。
    pub fn vb_poll_set(&self) -> Option<Arc<PollSet>> {
        self.driver.lock().vb_poll_set()
    }

    /// 获取事件唤醒源。
    pub fn event_poll_set(&self) -> Arc<PollSet> {
        Arc::clone(&self.event_poll_rx)
    }

    /// 打开设备。
    pub fn open_fh(&self) {
        let mut fh_guard = self.fh.lock();
        if fh_guard.is_none() {
            *fh_guard = Some(V4l2Fh::new());
        }
        drop(fh_guard);
        self.open_count.fetch_add(1, Ordering::SeqCst);
    }

    /// 关闭设备。
    pub fn close_fh(&self) {
        let prev = self.open_count.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 {
            self.driver.lock().release();
            *self.fh.lock() = None;
        } else if prev == 0 {
            self.open_count.store(0, Ordering::SeqCst);
        }
    }

    /// 投递事件。
    pub fn queue_event(&self, ev: &mut Event) {
        let mut fh_guard = self.fh.lock();
        if let Some(fh) = &mut *fh_guard
            && fh.queue_event(*ev) != QueueOutcome::NoSubscription
        {
            self.event_poll_rx.wake_from_irq(IoEvents::PRI);
        }
    }

    /// 是否有待处理事件。
    pub fn has_pending_events(&self) -> bool {
        self.fh.lock().as_ref().is_some_and(|fh| fh.pending() > 0)
    }

    /// 处理 G_EXT_CTRLS。
    pub fn handle_g_ext_ctrls(&self, header: &mut ExtControls, payload: &mut [u8]) -> Result<()> {
        self.handle_ext_ctrls(header, payload, CtrlHandler::g_ext_ctrls)
    }

    /// 处理 S_EXT_CTRLS。
    pub fn handle_s_ext_ctrls(&self, header: &mut ExtControls, payload: &mut [u8]) -> Result<()> {
        self.handle_ext_ctrls(header, payload, CtrlHandler::s_ext_ctrls)
    }

    /// 处理 TRY_EXT_CTRLS。
    pub fn handle_try_ext_ctrls(&self, header: &mut ExtControls, payload: &mut [u8]) -> Result<()> {
        self.handle_ext_ctrls(header, payload, CtrlHandler::try_ext_ctrls)
    }

    /// 扩展控件通用路径。
    fn handle_ext_ctrls(
        &self,
        header: &mut ExtControls,
        payload: &mut [u8],
        op: impl FnOnce(&CtrlHandler, &mut ExtControls, &mut [ExtControl]) -> Result<()>,
    ) -> Result<()> {
        let mut controls = parse_ext_controls(payload)?;
        let driver = self.driver.lock();
        let handler = driver.ctrl_handler().ok_or(V4l2Error::NotSupported)?;
        op(handler, header, &mut controls)?;
        write_ext_controls(payload, &controls);
        Ok(())
    }
}

/// 解析扩展控件。
fn parse_ext_controls(payload: &[u8]) -> Result<Vec<ExtControl>> {
    let ec_size = core::mem::size_of::<ExtControl>();
    if !payload.len().is_multiple_of(ec_size) {
        return Err(V4l2Error::InvalidArgument);
    }
    // SAFETY: payload 长度是 ExtControl 大小的整数倍；ExtControl 为 repr(C) POD。
    let src = unsafe {
        core::slice::from_raw_parts(
            payload.as_ptr() as *const ExtControl,
            payload.len() / ec_size,
        )
    };
    Ok(src.to_vec())
}

/// 写回扩展控件。
fn write_ext_controls(payload: &mut [u8], controls: &[ExtControl]) {
    debug_assert!(payload.len() == core::mem::size_of_val(controls));
    // SAFETY: 调用方保证 payload 长度与 controls 项数匹配。
    let dst = unsafe {
        core::slice::from_raw_parts_mut(payload.as_mut_ptr() as *mut ExtControl, controls.len())
    };
    dst.copy_from_slice(controls);
}

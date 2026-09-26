//! V4L2 设备抽象。

use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::any::Any;

use ax_sync::Mutex;
use axpoll::IoEvents;
use axpoll_set::PollSet;

use crate::{
    Result, V4l2Error,
    ctrls::CtrlHandler,
    driver::V4L2DriverOps,
    filehandler::{QueueOutcome, V4l2Fh},
    interface::{
        ctrl::{ExtControl, ExtControls},
        event::{Event, EventSubscription},
    },
    ioctl::{IoctlCmd, IoctlDispatcher, LegacyIoctlCmd, VideoIoctl},
};

/// V4L2 视频设备。
pub struct VideoDevice {
    driver: Arc<Mutex<dyn V4L2DriverOps>>,
    dispatcher: Mutex<IoctlDispatcher>,
    name: &'static str,
    sessions: Mutex<Sessions>,
}

struct Sessions {
    files: Vec<Weak<VideoFile>>,
    exclusive: bool,
    owner: Option<Weak<VideoFile>>,
}

/// State owned by one open video file description. Duplicated descriptors share it.
pub struct VideoFile {
    fh: Mutex<V4l2Fh>,
    event_poll: Arc<PollSet>,
}

impl VideoFile {
    /// The event readiness source for this file description.
    pub fn event_poll_set(&self) -> &Arc<PollSet> {
        &self.event_poll
    }

    /// Whether this file description has a pending event.
    pub fn has_pending_events(&self) -> bool {
        self.fh.lock().pending() > 0
    }
}

impl VideoDevice {
    /// 创建设备。
    pub fn new(driver: Arc<Mutex<dyn V4L2DriverOps>>, name: &'static str) -> Self {
        Self {
            driver,
            dispatcher: Mutex::new(IoctlDispatcher::new()),
            name,
            sessions: Mutex::new(Sessions {
                files: Vec::new(),
                exclusive: false,
                owner: None,
            }),
        }
    }

    /// 获取设备名。
    pub fn name(&self) -> &str {
        self.name
    }

    /// 处理 ioctl。
    pub fn handle_ioctl(
        &self,
        file: &Arc<VideoFile>,
        cmd: VideoIoctl,
        arg: &mut [u8],
    ) -> Result<()> {
        if needs_priority(cmd) {
            self.check_priority(file)?;
        }
        if let VideoIoctl::Modern(c) = cmd {
            match c {
                IoctlCmd::SubscribeEvent => {
                    let sub: EventSubscription = unsafe { crate::ioctl::read_from_bytes(arg) };
                    let mut driver = self.driver.lock();
                    let mut fh = file.fh.lock();
                    driver.subscribe_event(&mut fh, &sub)?;
                    if fh.pending() > 0 {
                        unsafe { file.event_poll.wake(IoEvents::PRI) };
                    }
                    return Ok(());
                }
                IoctlCmd::UnsubscribeEvent => {
                    let sub: EventSubscription = unsafe { crate::ioctl::read_from_bytes(arg) };
                    let mut driver = self.driver.lock();
                    let mut fh = file.fh.lock();
                    driver.unsubscribe_event(&mut fh, &sub)?;
                    return Ok(());
                }
                IoctlCmd::DQEvent => {
                    let mut ev: Event = unsafe { crate::ioctl::read_from_bytes(arg) };
                    let mut driver = self.driver.lock();
                    let mut fh = file.fh.lock();
                    driver.dqevent(&mut fh, &mut ev)?;
                    unsafe { crate::ioctl::write_to_bytes(arg, &ev) };
                    return Ok(());
                }
                IoctlCmd::GPriority => {
                    let p = self.max_priority();
                    unsafe { crate::ioctl::write_to_bytes(arg, &p) };
                    return Ok(());
                }
                IoctlCmd::SPriority => {
                    let p: u32 = unsafe { crate::ioctl::read_from_bytes(arg) };
                    if !(1..=3).contains(&p) {
                        return Err(crate::V4l2Error::InvalidArgument);
                    }
                    file.fh.lock().set_prio(p);
                    return Ok(());
                }
                _ => {}
            }
        }
        let queue_op = matches!(
            cmd,
            VideoIoctl::Modern(
                IoctlCmd::ReqBufs
                    | IoctlCmd::CreateBufs
                    | IoctlCmd::PrepareBuf
                    | IoctlCmd::RemoveBufs
                    | IoctlCmd::ExpBuf
                    | IoctlCmd::QBuf
                    | IoctlCmd::DQBuf
                    | IoctlCmd::StreamOn
                    | IoctlCmd::StreamOff
            )
        );
        let config_op = matches!(cmd, VideoIoctl::Modern(IoctlCmd::SFmt | IoctlCmd::SParm));
        if config_op
            && self
                .sessions
                .lock()
                .owner
                .as_ref()
                .is_some_and(|owner| !owner.ptr_eq(&Arc::downgrade(file)))
        {
            return Err(V4l2Error::Busy);
        }
        let was_unowned = if queue_op {
            let mut sessions = self.sessions.lock();
            if sessions
                .owner
                .as_ref()
                .is_some_and(|owner| !owner.ptr_eq(&Arc::downgrade(file)))
            {
                return Err(V4l2Error::Busy);
            }
            let vacant = sessions.owner.is_none();
            if vacant {
                // The weak owner does not extend this file's lifetime.
                sessions.owner = Some(Arc::downgrade(file));
            }
            vacant
        } else {
            false
        };
        let result = {
            let mut driver = self.driver.lock();
            self.dispatcher.lock().dispatch(&mut *driver, cmd, arg)
        };
        if queue_op
            && (result.is_err() && was_unowned
                || result.is_ok()
                    && matches!(cmd, VideoIoctl::Modern(IoctlCmd::ReqBufs))
                    && arg.get(..4).is_some_and(|bytes| bytes == [0, 0, 0, 0]))
        {
            let mut sessions = self.sessions.lock();
            if sessions
                .owner
                .as_ref()
                .is_some_and(|owner| owner.ptr_eq(&Arc::downgrade(file)))
            {
                sessions.owner = None;
            }
        }
        result
    }

    fn max_priority(&self) -> u32 {
        self.sessions
            .lock()
            .files
            .iter()
            .filter_map(Weak::upgrade)
            .map(|file| file.fh.lock().prio())
            .max()
            .unwrap_or(0)
    }

    /// Reject configuration changes by a file below the current device priority.
    pub fn check_priority(&self, file: &Arc<VideoFile>) -> Result<()> {
        let local = file.fh.lock().prio();
        if local < self.max_priority() {
            Err(V4l2Error::Busy)
        } else {
            Ok(())
        }
    }

    /// 禁用指定 ioctl。
    pub fn disable_ioctl(&self, cmd: u32) {
        self.dispatcher.lock().disable_cmd(cmd);
    }

    /// mmap 查询。
    pub fn mmap(
        &self,
        offset: u64,
        length: u64,
    ) -> Option<(Vec<usize>, Arc<dyn Any + Send + Sync>)> {
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

    /// Open a new description and acquire the hardware on the first open.
    pub fn open_fh(&self, exclusive: bool) -> Result<Arc<VideoFile>> {
        let mut sessions = self.sessions.lock();
        sessions.files.retain(|file| file.strong_count() > 0);
        if sessions.exclusive || (exclusive && !sessions.files.is_empty()) {
            return Err(V4l2Error::Busy);
        }
        if sessions.files.is_empty() {
            self.driver.lock().open()?;
        }
        let file = Arc::new(VideoFile {
            fh: Mutex::new(V4l2Fh::new()),
            event_poll: Arc::new(PollSet::new()),
        });
        sessions.files.push(Arc::downgrade(&file));
        sessions.exclusive = exclusive;
        Ok(file)
    }

    /// Close the description after its final descriptor reference is dropped.
    pub fn close_fh(&self, file: &Arc<VideoFile>) {
        let mut sessions = self.sessions.lock();
        sessions
            .files
            .retain(|entry| !entry.ptr_eq(&Arc::downgrade(file)) && entry.strong_count() > 0);
        let was_owner = sessions
            .owner
            .as_ref()
            .is_some_and(|owner| owner.ptr_eq(&Arc::downgrade(file)));
        if was_owner {
            sessions.owner = None;
        }
        if sessions.files.is_empty() {
            self.driver.lock().release();
            sessions.exclusive = false;
        } else if was_owner {
            self.driver.lock().close_owner();
        }
    }

    /// 投递事件。
    pub fn queue_event(&self, ev: &Event) {
        let files = self
            .sessions
            .lock()
            .files
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        for file in files {
            if file.fh.lock().queue_event(*ev) != QueueOutcome::NoSubscription {
                unsafe { file.event_poll.wake(IoEvents::PRI) };
            }
        }
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
        op(&handler.lock(), header, &mut controls)?;
        write_ext_controls(payload, &controls);
        Ok(())
    }
}

fn needs_priority(cmd: VideoIoctl) -> bool {
    match cmd {
        VideoIoctl::Modern(cmd) => matches!(
            cmd,
            IoctlCmd::SFmt
                | IoctlCmd::ReqBufs
                | IoctlCmd::StreamOn
                | IoctlCmd::StreamOff
                | IoctlCmd::SParm
                | IoctlCmd::SInput
                | IoctlCmd::SOutput
                | IoctlCmd::SEdid
                | IoctlCmd::SSelection
                | IoctlCmd::SPriority
                | IoctlCmd::SExtCtrls
                | IoctlCmd::SDvTimings
                | IoctlCmd::CreateBufs
                | IoctlCmd::RemoveBufs
        ),
        VideoIoctl::Legacy(cmd) => matches!(
            cmd,
            LegacyIoctlCmd::SFbuf
                | LegacyIoctlCmd::Overlay
                | LegacyIoctlCmd::SStd
                | LegacyIoctlCmd::SCtrl
                | LegacyIoctlCmd::STuner
                | LegacyIoctlCmd::SAudio
                | LegacyIoctlCmd::SAudioOut
                | LegacyIoctlCmd::SModulator
                | LegacyIoctlCmd::SFrequency
                | LegacyIoctlCmd::SCrop
                | LegacyIoctlCmd::SJpegComp
                | LegacyIoctlCmd::EncoderCmd
                | LegacyIoctlCmd::DecoderCmd
                | LegacyIoctlCmd::SHwFreqSeek
        ),
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

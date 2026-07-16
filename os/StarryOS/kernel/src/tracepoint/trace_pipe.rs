use core::{future::poll_fn, task::Poll};

use ax_sync::SpinMutex;
use axfs_ng_vfs::VfsResult;
use ktracepoint::TracePipeOps;

use crate::{
    pseudofs::DirectRwFsFileOps,
    task::{
        current_user_task,
        future::{block_on_user, interruptible_for},
    },
};

/// File representing the trace pipe.
///
/// TODO: Linux rejects concurrent `trace_pipe` readers at open time with
/// `EBUSY`, because this file consumes records from the shared trace buffer.
/// The current pseudofs/VFS path has no per-open hook or private file state, so
/// this node cannot faithfully reserve and release a reader slot yet. Keep the
/// limitation documented here until tracefs files can move their read state to
/// open-file private data.
pub struct TracePipeFile(SpinMutex<super::TextDrain>);

impl TracePipeFile {
    /// Creates a new `TracePipeFile` instance.
    pub const fn new() -> Self {
        Self(SpinMutex::new(super::TextDrain::new()))
    }

    fn readable(&self) -> bool {
        let trace_raw_pipe = super::TRACE_STATE.raw_pipe.lock();
        !trace_raw_pipe.is_empty()
    }
}

impl DirectRwFsFileOps for TracePipeFile {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let read_len = loop {
            {
                let mut drain = self.0.lock();
                let mut trace_raw_pipe = super::TRACE_STATE.raw_pipe.lock();
                let read_len = super::common_trace_pipe_read(&mut *trace_raw_pipe, &mut drain, buf);
                if read_len != 0 {
                    break read_len;
                }
            }

            // wait for new data
            let task = current_user_task();
            let _result = block_on_user(
                &task,
                interruptible_for(
                    &task,
                    poll_fn(|cx| {
                        match self.readable() {
                            true => Poll::Ready(true),
                            false => {
                                // Registration happens from trace_pipe read task context.
                                unsafe {
                                    super::TRACE_STATE
                                        .pipe_event
                                        .register(cx.waker(), axpoll::IoEvents::IN)
                                };
                                if self.readable() {
                                    Poll::Ready(true)
                                } else {
                                    Poll::Pending
                                }
                            }
                        }
                    }),
                ),
            )?;
        };
        Ok(read_len)
    }
}

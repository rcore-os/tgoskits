use core::{future::poll_fn, task::Poll};

use ax_sync::Mutex;
use ax_task::{
    current,
    future::{block_on, interruptible},
};
use axfs_ng_vfs::VfsResult;
use ktracepoint::TracePipeOps;

use crate::{pseudofs::DirectRwFsFileOps, task::AsThread, tracepoint::TRACE_RAW_PIPE};

/// File representing the trace pipe.
///
/// TODO: Linux rejects concurrent `trace_pipe` readers at open time with
/// `EBUSY`, because this file consumes records from the shared trace buffer.
/// The current pseudofs/VFS path has no per-open hook or private file state, so
/// this node cannot faithfully reserve and release a reader slot yet. Keep the
/// limitation documented here until tracefs files can move their read state to
/// open-file private data.
pub struct TracePipeFile(Mutex<super::TextDrain>);

impl TracePipeFile {
    /// Creates a new `TracePipeFile` instance.
    pub const fn new() -> Self {
        Self(Mutex::new(super::TextDrain::new()))
    }

    fn readable(&self) -> bool {
        let trace_raw_pipe = TRACE_RAW_PIPE.lock();
        !trace_raw_pipe.is_empty()
    }
}

impl DirectRwFsFileOps for TracePipeFile {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let curr = current();
        let proc_data = &curr.as_thread().proc_data;

        let read_len = loop {
            {
                let mut drain = self.0.lock();
                let mut trace_raw_pipe = TRACE_RAW_PIPE.lock();
                let read_len = super::common_trace_pipe_read(&mut *trace_raw_pipe, &mut drain, buf);
                if read_len != 0 {
                    break read_len;
                }
            }

            // wait for new data
            let _result = block_on(interruptible(poll_fn(|cx| {
                if self.readable() {
                    Poll::Ready(true)
                } else {
                    proc_data.child_exit_event.register(cx.waker());
                    Poll::Pending
                }
            })))?;
        };
        Ok(read_len)
    }
}

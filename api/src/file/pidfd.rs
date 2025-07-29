use alloc::sync::{Arc, Weak};

use axerrno::{LinuxError, LinuxResult};
use axio::PollState;
use axtask::future::block_on_interruptible;
use event_listener::{Event, listener};
use starry_core::task::ProcessData;

use crate::file::{FileLike, Kstat};

pub struct PidFd {
    proc_data: Weak<ProcessData>,
    exit_event: Arc<Event>,
}
impl PidFd {
    pub fn new(proc_data: &Arc<ProcessData>) -> Self {
        Self {
            proc_data: Arc::downgrade(proc_data),
            exit_event: proc_data.exit_event.clone(),
        }
    }

    pub fn process_data(&self) -> LinuxResult<Arc<ProcessData>> {
        self.proc_data.upgrade().ok_or(LinuxError::ESRCH)
    }
}
impl FileLike for PidFd {
    fn read(&self, _buf: &mut [u8]) -> LinuxResult<usize> {
        Err(LinuxError::EINVAL)
    }

    fn write(&self, _buf: &[u8]) -> LinuxResult<usize> {
        Err(LinuxError::EINVAL)
    }

    fn stat(&self) -> LinuxResult<Kstat> {
        Ok(Kstat::default())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }

    fn poll(&self) -> LinuxResult<PollState> {
        if self.proc_data.upgrade().is_none() {
            return Ok(PollState {
                readable: true,
                writable: false,
            });
        }

        listener!(self.exit_event => listener);

        if self.proc_data.upgrade().is_none() {
            return Ok(PollState {
                readable: true,
                writable: false,
            });
        }

        block_on_interruptible(async {
            listener.await;
            Ok(())
        })?;

        Ok(PollState {
            readable: true,
            writable: false,
        })
    }
}

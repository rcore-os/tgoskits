use ax_io::{BufReader, IoResult, prelude::*};
#[cfg(feature = "fd")]
use {crate::PosixError, crate::PosixResult, alloc::sync::Arc, ax_io::PollState};

use crate::sync::Mutex;

fn console_read_bytes(buf: &mut [u8]) -> IoResult<usize> {
    Ok(ax_api::stdio::ax_console_read_bytes(buf)?)
}

fn console_write_bytes(buf: &[u8]) -> IoResult<usize> {
    Ok(ax_api::stdio::ax_console_write_bytes(buf)?)
}

struct StdinRaw;
struct StdoutRaw;

impl Read for StdinRaw {
    // Non-blocking read, returns number of bytes read.
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        let mut read_len = 0;
        while read_len < buf.len() {
            let len = console_read_bytes(buf[read_len..].as_mut())?;
            if len == 0 {
                break;
            }
            read_len += len;
        }
        Ok(read_len)
    }
}

impl Write for StdoutRaw {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        console_write_bytes(buf)
    }

    fn flush(&mut self) -> IoResult {
        ax_api::stdio::ax_console_flush()?;
        Ok(())
    }
}

pub struct Stdin {
    inner: &'static Mutex<BufReader<StdinRaw>>,
}

impl Stdin {
    // Block until at least one byte is read.
    fn read_blocked(&self, buf: &mut [u8]) -> IoResult<usize> {
        let read_len = self.inner.lock().read(buf)?;
        if buf.is_empty() || read_len > 0 {
            return Ok(read_len);
        }
        // Sleep until the runtime RX worker publishes progress, then retry.
        loop {
            ax_api::stdio::ax_console_wait_readable()?;
            let read_len = self.inner.lock().read(buf)?;
            if read_len > 0 {
                return Ok(read_len);
            }
        }
    }
}

impl Read for Stdin {
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        self.read_blocked(buf)
    }
}

pub struct Stdout {
    inner: &'static Mutex<StdoutRaw>,
}

impl Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        self.inner.lock().write(buf)
    }

    fn flush(&mut self) -> IoResult {
        self.inner.lock().flush()
    }
}

/// Constructs a new handle to the standard input of the current process.
pub fn stdin() -> Stdin {
    static INSTANCE: ax_lazyinit::OnceLock<Mutex<BufReader<StdinRaw>>> =
        ax_lazyinit::OnceLock::new();
    Stdin {
        inner: INSTANCE.call_once(|| Mutex::new(BufReader::new(StdinRaw))),
    }
}

/// Constructs a new handle to the standard output of the current process.
pub fn stdout() -> Stdout {
    static INSTANCE: Mutex<StdoutRaw> = Mutex::new(StdoutRaw);
    Stdout { inner: &INSTANCE }
}

#[cfg(feature = "fd")]
impl super::fd_ops::FileLike for Stdin {
    fn read(&self, buf: &mut [u8]) -> PosixResult<usize> {
        Ok(self.read_blocked(buf)?)
    }

    fn write(&self, _buf: &[u8]) -> PosixResult<usize> {
        Err(PosixError::EPERM)
    }

    fn stat(&self) -> PosixResult<crate::ctypes::stat> {
        let st_mode = 0o20000 | 0o440u32; // S_IFCHR | r--r-----
        Ok(crate::ctypes::stat {
            st_ino: 1,
            st_nlink: 1,
            st_mode,
            ..Default::default()
        })
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }

    fn poll(&self) -> PosixResult<PollState> {
        Ok(PollState {
            readable: true,
            writable: true,
            readiness_version: 0,
        })
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> PosixResult {
        Ok(())
    }
}

#[cfg(feature = "fd")]
impl super::fd_ops::FileLike for Stdout {
    fn read(&self, _buf: &mut [u8]) -> PosixResult<usize> {
        Err(PosixError::EPERM)
    }

    fn write(&self, buf: &[u8]) -> PosixResult<usize> {
        Ok(self.inner.lock().write(buf)?)
    }

    fn stat(&self) -> PosixResult<crate::ctypes::stat> {
        let st_mode = 0o20000 | 0o220u32; // S_IFCHR | -w--w----
        Ok(crate::ctypes::stat {
            st_ino: 1,
            st_nlink: 1,
            st_mode,
            ..Default::default()
        })
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }

    fn poll(&self) -> PosixResult<PollState> {
        Ok(PollState {
            readable: true,
            writable: true,
            readiness_version: 0,
        })
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> PosixResult {
        Ok(())
    }
}

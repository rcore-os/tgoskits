use core::any::Any;

use alloc::sync::Arc;
use axerrno::{LinuxError, LinuxResult};
use axio::{BufReader, PollState, prelude::*};
use axsync::Mutex;
use linux_raw_sys::general::S_IFCHR;

use super::Kstat;

struct StdinRaw;

impl Read for StdinRaw {
    // Non-blocking read, returns number of bytes read.
    fn read(&mut self, buf: &mut [u8]) -> LinuxResult<usize> {
        let mut read_len = 0;
        while read_len < buf.len() {
            let len = axhal::console::read_bytes(&mut buf[read_len..]);
            if len == 0 {
                break;
            }
            read_len += len;

            for c in &mut buf[..len] {
                if *c == b'\r' {
                    *c = b'\n';
                }
            }
        }
        Ok(read_len)
    }
}

struct StdoutRaw;

impl Write for StdoutRaw {
    fn write(&mut self, buf: &[u8]) -> LinuxResult<usize> {
        axhal::console::write_bytes(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> LinuxResult {
        Ok(())
    }
}

pub struct Stdin {
    inner: &'static Mutex<BufReader<StdinRaw>>,
}

impl Stdin {
    // Block until at least one byte is read.
    fn read_blocked(&self, buf: &mut [u8]) -> LinuxResult<usize> {
        let read_len = self.inner.lock().read(buf)?;
        if buf.is_empty() || read_len > 0 {
            return Ok(read_len);
        }
        // try again until we get something
        loop {
            let read_len = self.inner.lock().read(buf)?;
            if read_len > 0 {
                return Ok(read_len);
            }
            axtask::yield_now();
        }
    }
}

impl Read for Stdin {
    fn read(&mut self, buf: &mut [u8]) -> LinuxResult<usize> {
        self.read_blocked(buf)
    }
}

pub struct Stdout {
    inner: &'static Mutex<StdoutRaw>,
}

impl Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> LinuxResult<usize> {
        self.inner.lock().write(buf)
    }

    fn flush(&mut self) -> LinuxResult {
        self.inner.lock().flush()
    }
}

/// Constructs a new handle to the standard input of the current process.
pub fn stdin() -> Stdin {
    static INSTANCE: Mutex<BufReader<StdinRaw>> = Mutex::new(BufReader::new(StdinRaw));
    Stdin { inner: &INSTANCE }
}

/// Constructs a new handle to the standard output of the current process.
pub fn stdout() -> Stdout {
    static INSTANCE: Mutex<StdoutRaw> = Mutex::new(StdoutRaw);
    Stdout { inner: &INSTANCE }
}

impl super::FileLike for Stdin {
    fn read(&self, buf: &mut [u8]) -> LinuxResult<usize> {
        Ok(self.read_blocked(buf)?)
    }

    fn write(&self, _buf: &[u8]) -> LinuxResult<usize> {
        Err(LinuxError::EPERM)
    }

    fn stat(&self) -> LinuxResult<Kstat> {
        Ok(Kstat {
            mode: S_IFCHR | 0o444u32, // r--r--r--
            ..Default::default()
        })
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn poll(&self) -> LinuxResult<PollState> {
        Ok(PollState {
            readable: true,
            writable: true,
        })
    }
}

impl super::FileLike for Stdout {
    fn read(&self, _buf: &mut [u8]) -> LinuxResult<usize> {
        Err(LinuxError::EPERM)
    }

    fn write(&self, buf: &[u8]) -> LinuxResult<usize> {
        Ok(self.inner.lock().write(buf)?)
    }

    fn stat(&self) -> LinuxResult<Kstat> {
        Ok(Kstat {
            mode: S_IFCHR | 0o220u32, // -w--w----
            ..Default::default()
        })
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn poll(&self) -> LinuxResult<PollState> {
        Ok(PollState {
            readable: true,
            writable: true,
        })
    }
}

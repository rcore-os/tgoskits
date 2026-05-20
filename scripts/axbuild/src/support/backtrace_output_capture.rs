//! Cross-platform stdout/stderr tee that writes only raw backtrace blocks to a log file.

use std::io;

#[cfg(unix)]
mod platform {
    use std::{
        fs::File,
        io::{self, Read, Write},
        os::unix::io::FromRawFd,
        sync::{Arc, Mutex},
        thread::JoinHandle,
    };

    use crate::backtrace::{
        BacktraceBlockCapture, BacktraceQemuCapture, flush_pending_stream_symbolize,
    };

    struct PipeEnds {
        read_fd: i32,
        write_fd: i32,
    }

    impl PipeEnds {
        fn new() -> io::Result<Self> {
            let mut fds = [0i32; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                read_fd: fds[0],
                write_fd: fds[1],
            })
        }

        fn into_read_fd(mut self) -> i32 {
            self.write_fd = -1;
            let fd = self.read_fd;
            self.read_fd = -1;
            fd
        }
    }

    impl Drop for PipeEnds {
        fn drop(&mut self) {
            for fd in [self.read_fd, self.write_fd] {
                if fd >= 0 {
                    unsafe { libc::close(fd) };
                }
            }
            self.read_fd = -1;
            self.write_fd = -1;
        }
    }

    struct InstallRollback {
        saved_stdout: i32,
        saved_stderr: i32,
        tee_out: i32,
        redirected: bool,
    }

    impl InstallRollback {
        fn new() -> io::Result<Self> {
            let saved_stdout = unsafe { libc::dup(libc::STDOUT_FILENO) };
            let saved_stderr = unsafe { libc::dup(libc::STDERR_FILENO) };
            if saved_stdout < 0 || saved_stderr < 0 {
                let err = io::Error::last_os_error();
                if saved_stdout >= 0 {
                    unsafe { libc::close(saved_stdout) };
                }
                if saved_stderr >= 0 {
                    unsafe { libc::close(saved_stderr) };
                }
                return Err(err);
            }
            let tee_out = unsafe { libc::dup(saved_stdout) };
            if tee_out < 0 {
                let err = io::Error::last_os_error();
                unsafe {
                    libc::close(saved_stdout);
                    libc::close(saved_stderr);
                }
                return Err(err);
            }
            Ok(Self {
                saved_stdout,
                saved_stderr,
                tee_out,
                redirected: false,
            })
        }

        fn restore_stdio(&self) {
            if self.redirected {
                unsafe {
                    libc::dup2(self.saved_stdout, libc::STDOUT_FILENO);
                    libc::dup2(self.saved_stderr, libc::STDERR_FILENO);
                }
            }
        }

        fn take_tee_out(&mut self) -> i32 {
            let fd = self.tee_out;
            self.tee_out = -1;
            fd
        }

        fn into_guard(self) -> (i32, i32) {
            let saved_stdout = self.saved_stdout;
            let saved_stderr = self.saved_stderr;
            std::mem::forget(self);
            (saved_stdout, saved_stderr)
        }
    }

    impl Drop for InstallRollback {
        fn drop(&mut self) {
            self.restore_stdio();
            for fd in [self.saved_stdout, self.saved_stderr, self.tee_out] {
                if fd >= 0 {
                    unsafe { libc::close(fd) };
                }
            }
        }
    }

    pub(super) struct PlatformGuard {
        saved_stdout: i32,
        saved_stderr: i32,
        capture: Arc<Mutex<BacktraceBlockCapture>>,
        captured_blocks: Arc<Mutex<Vec<Vec<String>>>>,
        stream_symbolize: Option<Arc<crate::backtrace::BacktraceSymbolizeSession>>,
        reader: Option<JoinHandle<io::Result<()>>>,
    }

    pub(super) fn install(capture: &BacktraceQemuCapture) -> io::Result<PlatformGuard> {
        let mut rollback = InstallRollback::new()?;
        let log_path = capture
            .write_log_during_capture
            .then_some(capture.log_path.as_path());
        let block_capture = Arc::new(Mutex::new(BacktraceBlockCapture::create(
            log_path,
            Some(capture.captured_blocks.clone()),
        )?));

        let mut pipe = PipeEnds::new()?;
        if unsafe { libc::dup2(pipe.write_fd, libc::STDOUT_FILENO) } < 0
            || unsafe { libc::dup2(pipe.write_fd, libc::STDERR_FILENO) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            libc::close(pipe.write_fd);
        }
        pipe.write_fd = -1;
        rollback.redirected = true;

        let pipe_read = pipe.into_read_fd();
        let tee_out = rollback.take_tee_out();
        let capture_for_reader = block_capture.clone();
        let suppress_terminal_raw_blocks = capture.suppress_terminal_raw_blocks;

        let reader = std::thread::spawn(move || {
            let mut pipe = unsafe { File::from_raw_fd(pipe_read) };
            let mut terminal = unsafe { File::from_raw_fd(tee_out) };
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        let terminal_chunk = if let Ok(mut capture) = capture_for_reader.lock() {
                            capture
                                .push_bytes_for_tee(chunk, suppress_terminal_raw_blocks)
                                .unwrap_or_else(|_| chunk.to_vec())
                        } else {
                            chunk.to_vec()
                        };
                        if !terminal_chunk.is_empty() {
                            let _ = terminal.write_all(&terminal_chunk);
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => return Err(err),
                }
            }
            let _ = terminal.flush();
            Ok(())
        });

        let (saved_stdout, saved_stderr) = rollback.into_guard();
        Ok(PlatformGuard {
            saved_stdout,
            saved_stderr,
            capture: block_capture,
            captured_blocks: capture.captured_blocks.clone(),
            stream_symbolize: capture.stream_symbolize.clone(),
            reader: Some(reader),
        })
    }

    impl Drop for PlatformGuard {
        fn drop(&mut self) {
            let _ = io::stdout().flush();
            let _ = io::stderr().flush();
            unsafe {
                libc::dup2(self.saved_stdout, libc::STDOUT_FILENO);
                libc::dup2(self.saved_stderr, libc::STDERR_FILENO);
                libc::close(self.saved_stdout);
                libc::close(self.saved_stderr);
            }
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
            if let Ok(mut capture) = self.capture.lock() {
                let _ = capture.finish();
            }
            if let Some(session) = &self.stream_symbolize {
                flush_pending_stream_symbolize(session, &self.captured_blocks);
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        fs::File,
        io::{self, Read, Write},
        os::windows::io::{FromRawHandle, IntoRawHandle},
        path::Path,
        sync::{Arc, Mutex},
        thread::JoinHandle,
    };

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
        System::{
            Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle},
            Pipes::CreatePipe,
        },
    };

    use crate::backtrace::{
        BacktraceBlockCapture, BacktraceQemuCapture, flush_pending_stream_symbolize,
    };

    pub(super) struct PlatformGuard {
        orig_stdout: HANDLE,
        orig_stderr: HANDLE,
        pipe_write: HANDLE,
        capture: Arc<Mutex<BacktraceBlockCapture>>,
        captured_blocks: Arc<Mutex<Vec<Vec<String>>>>,
        stream_symbolize: Option<Arc<crate::backtrace::BacktraceSymbolizeSession>>,
        reader: Option<JoinHandle<io::Result<()>>>,
    }

    pub(super) fn install(capture: &BacktraceQemuCapture) -> io::Result<PlatformGuard> {
        let mut read_handle = INVALID_HANDLE_VALUE;
        let mut write_handle = INVALID_HANDLE_VALUE;
        if unsafe { CreatePipe(&mut read_handle, &mut write_handle, std::ptr::null_mut(), 0) } == 0
        {
            return Err(io::Error::last_os_error());
        }

        let orig_stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let orig_stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        if orig_stdout == INVALID_HANDLE_VALUE || orig_stderr == INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(read_handle);
                CloseHandle(write_handle);
            }
            return Err(io::Error::last_os_error());
        }

        if unsafe { SetStdHandle(STD_OUTPUT_HANDLE, write_handle) } == 0
            || unsafe { SetStdHandle(STD_ERROR_HANDLE, write_handle) } == 0
        {
            unsafe {
                CloseHandle(read_handle);
                CloseHandle(write_handle);
            }
            return Err(io::Error::last_os_error());
        }

        let log_path = capture
            .write_log_during_capture
            .then_some(capture.log_path.as_path());
        let block_capture = Arc::new(Mutex::new(BacktraceBlockCapture::create(
            log_path,
            Some(capture.captured_blocks.clone()),
        )?));
        let capture_for_reader = block_capture.clone();
        let suppress_terminal_raw_blocks = capture.suppress_terminal_raw_blocks;

        let reader = std::thread::spawn(move || {
            let mut pipe = unsafe { File::from_raw_handle(read_handle as _) };
            let mut terminal = unsafe { File::from_raw_handle(orig_stdout as _) };
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        let terminal_chunk = if let Ok(mut capture) = capture_for_reader.lock() {
                            capture
                                .push_bytes_for_tee(chunk, suppress_terminal_raw_blocks)
                                .unwrap_or_else(|_| chunk.to_vec())
                        } else {
                            chunk.to_vec()
                        };
                        if !terminal_chunk.is_empty() {
                            let _ = terminal.write_all(&terminal_chunk);
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => return Err(err),
                }
            }
            let _ = terminal.flush();
            Ok(())
        });

        Ok(PlatformGuard {
            orig_stdout,
            orig_stderr,
            pipe_write: write_handle,
            capture: block_capture,
            captured_blocks: capture.captured_blocks.clone(),
            stream_symbolize: capture.stream_symbolize.clone(),
            reader: Some(reader),
        })
    }

    impl Drop for PlatformGuard {
        fn drop(&mut self) {
            let _ = io::stdout().flush();
            let _ = io::stderr().flush();
            unsafe {
                SetStdHandle(STD_OUTPUT_HANDLE, self.orig_stdout);
                SetStdHandle(STD_ERROR_HANDLE, self.orig_stderr);
                CloseHandle(self.pipe_write);
            }
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
            if let Ok(mut capture) = self.capture.lock() {
                let _ = capture.finish();
            }
            if let Some(session) = &self.stream_symbolize {
                flush_pending_stream_symbolize(session, &self.captured_blocks);
            }
        }
    }
}

pub(crate) struct BacktraceOutputCaptureGuard {
    #[allow(dead_code)]
    inner: platform::PlatformGuard,
}

impl BacktraceOutputCaptureGuard {
    pub(crate) fn install(capture: &crate::backtrace::BacktraceQemuCapture) -> io::Result<Self> {
        Ok(Self {
            inner: platform::install(capture)?,
        })
    }
}

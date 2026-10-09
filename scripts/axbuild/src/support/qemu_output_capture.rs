//! Cross-platform stdout/stderr tee used only for QEMU success contracts.
//!
//! The target's backtrace text is forwarded unchanged.  This module does not
//! retain blocks, write diagnostic logs, or resolve symbols on the host.

use std::io;

#[cfg(unix)]
mod platform {
    use std::{
        fs::File,
        io::{self, Read, Write},
        os::unix::io::FromRawFd,
        thread::JoinHandle,
    };

    use crate::support::qemu_success::QemuSuccessOutput;

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
                let error = io::Error::last_os_error();
                if saved_stdout >= 0 {
                    unsafe { libc::close(saved_stdout) };
                }
                if saved_stderr >= 0 {
                    unsafe { libc::close(saved_stderr) };
                }
                return Err(error);
            }
            let tee_out = unsafe { libc::dup(saved_stdout) };
            if tee_out < 0 {
                let error = io::Error::last_os_error();
                unsafe {
                    libc::close(saved_stdout);
                    libc::close(saved_stderr);
                }
                return Err(error);
            }
            Ok(Self {
                saved_stdout,
                saved_stderr,
                tee_out,
                redirected: false,
            })
        }

        fn take_tee_out(&mut self) -> i32 {
            let fd = self.tee_out;
            self.tee_out = -1;
            fd
        }

        fn into_guard(mut self) -> (i32, i32) {
            self.redirected = false;
            let values = (self.saved_stdout, self.saved_stderr);
            std::mem::forget(self);
            values
        }
    }

    impl Drop for InstallRollback {
        fn drop(&mut self) {
            if self.redirected {
                unsafe {
                    libc::dup2(self.saved_stdout, libc::STDOUT_FILENO);
                    libc::dup2(self.saved_stderr, libc::STDERR_FILENO);
                }
            }
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
        reader: Option<JoinHandle<io::Result<()>>>,
    }

    pub(super) fn install(success_output: Option<QemuSuccessOutput>) -> io::Result<PlatformGuard> {
        let mut rollback = InstallRollback::new()?;
        let mut pipe = PipeEnds::new()?;
        if unsafe { libc::dup2(pipe.write_fd, libc::STDOUT_FILENO) } < 0
            || unsafe { libc::dup2(pipe.write_fd, libc::STDERR_FILENO) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        unsafe { libc::close(pipe.write_fd) };
        pipe.write_fd = -1;
        rollback.redirected = true;

        let pipe_read = pipe.into_read_fd();
        let tee_out = rollback.take_tee_out();
        let reader = std::thread::spawn(move || {
            let mut pipe = unsafe { File::from_raw_fd(pipe_read) };
            let mut terminal = unsafe { File::from_raw_fd(tee_out) };
            let mut buffer = [0u8; 8192];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let chunk = &buffer[..count];
                        if let Some(output) = &success_output {
                            output.append(chunk);
                        }
                        terminal.write_all(chunk)?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            terminal.flush()
        });
        let (saved_stdout, saved_stderr) = rollback.into_guard();
        Ok(PlatformGuard {
            saved_stdout,
            saved_stderr,
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
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        fs::File,
        io::{self, Read, Write},
        os::windows::io::FromRawHandle,
        thread::JoinHandle,
    };

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
        System::{
            Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle},
            Pipes::CreatePipe,
        },
    };

    use crate::support::qemu_success::QemuSuccessOutput;

    pub(super) struct PlatformGuard {
        orig_stdout: HANDLE,
        orig_stderr: HANDLE,
        pipe_write: HANDLE,
        reader: Option<JoinHandle<io::Result<()>>>,
    }

    pub(super) fn install(success_output: Option<QemuSuccessOutput>) -> io::Result<PlatformGuard> {
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
        let reader = std::thread::spawn(move || {
            let mut pipe = unsafe { File::from_raw_handle(read_handle as _) };
            let mut terminal = unsafe { File::from_raw_handle(orig_stdout as _) };
            let mut buffer = [0u8; 8192];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let chunk = &buffer[..count];
                        if let Some(output) = &success_output {
                            output.append(chunk);
                        }
                        terminal.write_all(chunk)?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            terminal.flush()
        });
        Ok(PlatformGuard {
            orig_stdout,
            orig_stderr,
            pipe_write: write_handle,
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
        }
    }
}

pub(crate) struct QemuOutputCaptureGuard {
    #[allow(dead_code)]
    inner: platform::PlatformGuard,
}

impl QemuOutputCaptureGuard {
    pub(crate) fn install(
        success_output: Option<crate::support::qemu_success::QemuSuccessOutput>,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: platform::install(success_output)?,
        })
    }
}

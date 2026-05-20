//! Tee process stdout/stderr to a log file while preserving terminal output (Unix only).

use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::unix::io::FromRawFd,
    path::Path,
    thread::JoinHandle,
};

struct PipeEnds {
    read_fd: i32,
    write_fd: i32,
}

impl PipeEnds {
    fn new() -> io::Result<Self> {
        let mut fds = [0i32; 2];
        // SAFETY: `pipe` fills two ints on success.
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
                // SAFETY: closing fds we own.
                unsafe { libc::close(fd) };
            }
        }
        self.read_fd = -1;
        self.write_fd = -1;
    }
}

/// Owns saved stdout/stderr dups; restores them on drop if `redirected`.
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
            // SAFETY: fds are valid dups of the original stdio.
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
                // SAFETY: closing fds we own.
                unsafe { libc::close(fd) };
            }
        }
    }
}

fn write_best_effort(file: &mut File, buf: &[u8]) {
    let _ = file.write_all(buf);
}

pub(crate) struct OutputTeeGuard {
    saved_stdout: i32,
    saved_stderr: i32,
    reader: Option<JoinHandle<io::Result<()>>>,
}

impl OutputTeeGuard {
    pub(crate) fn install(log_path: &Path) -> io::Result<Self> {
        let mut rollback = InstallRollback::new()?;

        let log_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(log_path)?;

        let mut pipe = PipeEnds::new()?;
        // SAFETY: dup2 to valid pipe write end.
        if unsafe { libc::dup2(pipe.write_fd, libc::STDOUT_FILENO) } < 0
            || unsafe { libc::dup2(pipe.write_fd, libc::STDERR_FILENO) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: write end is installed on stdio; close the extra reference.
        unsafe {
            libc::close(pipe.write_fd);
        }
        pipe.write_fd = -1;
        rollback.redirected = true;

        let pipe_read = pipe.into_read_fd();
        let tee_out = rollback.take_tee_out();

        let reader = std::thread::spawn(move || {
            let mut pipe = unsafe { File::from_raw_fd(pipe_read) };
            let mut terminal = unsafe { File::from_raw_fd(tee_out) };
            let mut log = log_file;
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        if log.write_all(chunk).is_err() {
                            return Err(io::Error::other("failed to write qemu log"));
                        }
                        write_best_effort(&mut terminal, chunk);
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => return Err(err),
                }
            }
            let _ = log.flush();
            let _ = terminal.flush();
            Ok(())
        });

        let (saved_stdout, saved_stderr) = rollback.into_guard();
        Ok(Self {
            saved_stdout,
            saved_stderr,
            reader: Some(reader),
        })
    }
}

impl Drop for OutputTeeGuard {
    fn drop(&mut self) {
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
        // SAFETY: restoring process stdio from saved dups.
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

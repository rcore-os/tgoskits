//! Tee process stdout/stderr to a log file while preserving terminal output (Unix only).

use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::unix::io::FromRawFd,
    path::Path,
    thread::JoinHandle,
};

pub(crate) struct OutputTeeGuard {
    saved_stdout: i32,
    saved_stderr: i32,
    reader: Option<JoinHandle<io::Result<()>>>,
}

impl OutputTeeGuard {
    pub(crate) fn install(log_path: &Path) -> io::Result<Self> {
        let mut pipe_fds = [0i32; 2];
        // SAFETY: `pipe` fills two ints on success.
        if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let pipe_read = pipe_fds[0];
        let pipe_write = pipe_fds[1];

        let saved_stdout = unsafe { libc::dup(libc::STDOUT_FILENO) };
        let saved_stderr = unsafe { libc::dup(libc::STDERR_FILENO) };
        if saved_stdout < 0 || saved_stderr < 0 {
            return Err(io::Error::last_os_error());
        }

        let tee_out = unsafe { libc::dup(saved_stdout) };
        if tee_out < 0 {
            return Err(io::Error::last_os_error());
        }

        if unsafe { libc::dup2(pipe_write, libc::STDOUT_FILENO) } < 0
            || unsafe { libc::dup2(pipe_write, libc::STDERR_FILENO) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            libc::close(pipe_write);
        }

        let log_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(log_path)?;

        let reader = std::thread::spawn(move || {
            let mut pipe = unsafe { File::from_raw_fd(pipe_read) };
            let mut terminal = unsafe { File::from_raw_fd(tee_out) };
            let mut log = log_file;
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        terminal.write_all(&buf[..n])?;
                        log.write_all(&buf[..n])?;
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => return Err(err),
                }
            }
            terminal.flush()?;
            log.flush()?;
            Ok(())
        });

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

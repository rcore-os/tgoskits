use core::ffi::c_int;

use ax_errno::LinuxError;
use ax_hal::time::wall_time;

use crate::{ctypes, imp::fd_ops::get_file_like};

const POLLIN_EVENT: i16 = ctypes::POLLIN as i16;
const POLLOUT_EVENT: i16 = ctypes::POLLOUT as i16;
const POLLERR_EVENT: i16 = ctypes::POLLERR as i16;
const POLLNVAL_EVENT: i16 = ctypes::POLLNVAL as i16;

/// Poll file descriptors for I/O readiness (POSIX `poll` semantics).
///
/// Returns the number of ready descriptors on success, 0 on timeout,
/// or a negative errno value on error.
pub fn sys_poll(fds: *mut ctypes::pollfd, nfds: ctypes::nfds_t, timeout: c_int) -> c_int {
    debug!(
        "sys_poll <= fds:{:#x} nfds:{} timeout:{}",
        fds as usize, nfds, timeout
    );
    syscall_body!(sys_poll, {
        if fds.is_null() && nfds > 0 {
            return Err(LinuxError::EFAULT);
        }

        let fds_slice = if nfds > 0 {
            unsafe { core::slice::from_raw_parts_mut(fds, nfds as _) }
        } else {
            &mut []
        };

        // Clear all revents
        for pfd in fds_slice.iter_mut() {
            pfd.revents = 0;
        }

        // Compute deadline
        let deadline = if timeout < 0 {
            None // block indefinitely
        } else if timeout == 0 {
            Some(wall_time()) // immediate, non-blocking
        } else {
            Some(wall_time() + core::time::Duration::from_millis(timeout as u64))
        };

        loop {
            #[cfg(feature = "net")]
            ax_net::request_poll();

            let mut ready_count: usize = 0;

            for pfd in fds_slice.iter_mut() {
                if pfd.fd < 0 {
                    // Negative fd: ignore (POSIX behavior)
                    continue;
                }

                match get_file_like(pfd.fd) {
                    Ok(file) => match file.poll() {
                        Ok(state) => {
                            let mut revents = 0;
                            if state.readable && (pfd.events & POLLIN_EVENT) != 0 {
                                revents |= POLLIN_EVENT;
                            }
                            if state.writable && (pfd.events & POLLOUT_EVENT) != 0 {
                                revents |= POLLOUT_EVENT;
                            }
                            if revents != 0 {
                                pfd.revents = revents;
                                ready_count += 1;
                            }
                        }
                        Err(_) => {
                            pfd.revents = POLLERR_EVENT;
                            ready_count += 1;
                        }
                    },
                    Err(_) => {
                        pfd.revents = POLLNVAL_EVENT;
                        ready_count += 1;
                    }
                }
            }

            if ready_count > 0 {
                return Ok(ready_count);
            }

            if deadline.is_some_and(|ddl| wall_time() >= ddl) {
                debug!("    poll timeout!");
                return Ok(0);
            }

            crate::sys_sched_yield();
        }
    })
}

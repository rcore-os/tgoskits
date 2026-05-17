use alloc::{format, string::String, sync::Arc};
use core::{
    ffi::c_char,
    sync::atomic::{AtomicU64, Ordering},
};

use ax_errno::{AxError, AxResult};
use ax_fs::{FS_CONTEXT, OpenOptions};
use ax_task::current;
use linux_raw_sys::general::{MFD_CLOEXEC, O_RDWR};

use crate::{
    file::{File, add_file_like, memfd::Memfd},
    mm::vm_load_string,
    task::AsThread,
};

/// `MFD_HUGETLB` — bit 2. We do not back memfds with hugepages; we accept
/// the flag (so musl/pipewire that probe with it succeed) and ignore.
const MFD_HUGETLB: u32 = 0x0004;
/// `MFD_NOEXEC_SEAL` — Linux 6.3+. Forces the W^X policy on the memfd.
/// We don't enforce executable mappings, so accepting and ignoring is
/// equivalent in our security model.
const MFD_NOEXEC_SEAL: u32 = 0x0008;
/// `MFD_EXEC` — opt-out from `MFD_NOEXEC_SEAL`, also Linux 6.3+. Same
/// reasoning: accepted and ignored.
const MFD_EXEC: u32 = 0x0010;

/// `MFD_ALLOW_SEALING` — bit 1. `linux-raw-sys` does not export it on every
/// target, so define locally.
const MFD_ALLOW_SEALING: u32 = 0x0002;

/// Linux enforces `NAME_MAX - strlen("memfd:")` = 249 bytes for the name.
const MEMFD_NAME_MAX: usize = 249;

/// Monotonic counter for backing-file names. Combined with the creator pid
/// and a retry loop, collisions within one process and across processes
/// that happen to share the same pid across reboots are avoided.
static MEMFD_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Max number of retries when an O_EXCL create collides.
const MAX_CREATE_RETRIES: u32 = 16;

pub fn sys_memfd_create(name: *const c_char, flags: u32) -> AxResult<isize> {
    let valid_flags = MFD_CLOEXEC | MFD_ALLOW_SEALING | MFD_HUGETLB | MFD_NOEXEC_SEAL | MFD_EXEC;
    if flags & !valid_flags != 0 {
        return Err(AxError::InvalidInput);
    }

    let cloexec = flags & MFD_CLOEXEC != 0;
    let allow_sealing = flags & MFD_ALLOW_SEALING != 0;

    // Load the name argument. Linux rejects overlong names and names
    // containing a '/'; do the same so callers fail loud instead of
    // silently accepting malformed input.
    let name_str: String = vm_load_string(name)?;
    if name_str.len() > MEMFD_NAME_MAX {
        return Err(AxError::InvalidInput);
    }
    if name_str.contains('/') {
        return Err(AxError::InvalidInput);
    }

    let pid = current().as_thread().proc_data.proc.pid();

    let mut last_err = AxError::AlreadyExists;
    for _ in 0..MAX_CREATE_RETRIES {
        let id = MEMFD_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/memfd-{:x}-{:016x}", pid as u32, id);

        let fs = FS_CONTEXT.lock().clone();
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .create_new(true)
            .open(&fs, &path)
        {
            Ok(open_result) => {
                let file = open_result.into_file()?;
                // Classic Unix anonymous-via-unlink: the fd keeps the
                // inode alive, but the directory entry is gone so
                // fstat(fd).st_nlink == 0 and no other process can
                // open the path by racing pid+counter prediction.
                // Matches Linux memfd semantics exactly.
                let _ = fs.remove_file(&path);
                let inner = Arc::new(File::new(file, O_RDWR));
                let memfd = Memfd::new(inner, name_str, allow_sealing);
                return add_file_like(memfd, cloexec).map(|fd| fd as _);
            }
            Err(AxError::AlreadyExists) => {
                last_err = AxError::AlreadyExists;
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err)
}

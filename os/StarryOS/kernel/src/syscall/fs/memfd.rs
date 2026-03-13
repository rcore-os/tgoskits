use alloc::format;
use core::ffi::c_char;

use axerrno::{AxError, AxResult};
use axfs::{FS_CONTEXT, OpenOptions};
use linux_raw_sys::general::MFD_CLOEXEC;

use crate::{
    file::{File, FileLike},
    mm::UserConstPtr,
};

// TODO: correct memfd implementation

pub fn sys_memfd_create(_name: UserConstPtr<c_char>, flags: u32) -> AxResult<isize> {
    // This is cursed
    for id in 0..0xffff {
        let name = format!("/tmp/memfd-{id:04x}");
        let fs = FS_CONTEXT.lock().clone();
        if fs.resolve(&name).is_err() {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(&fs, &name)?
                .into_file()?;
            let cloexec = flags & MFD_CLOEXEC != 0;
            return File::new(file).add_to_fd_table(cloexec).map(|fd| fd as _);
        }
    }
    Err(AxError::TooManyOpenFiles)
}

use axerrno::LinuxResult;
use axfs_ng::{FS_CONTEXT};

pub fn init_etc() -> LinuxResult<()> {
    let fs = FS_CONTEXT.lock();
    fs.write("/etc/passwd", "nobody:x:0:0::/musl:/bin/sh\n")?;

    Ok(())
}

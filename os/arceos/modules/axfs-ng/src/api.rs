use alloc::string::{String, ToString};

use ax_errno::AxResult;
use axfs_ng_vfs::NodePermission;

use crate::highlevel::FS_CONTEXT;

pub fn create_dir(path: &str) -> AxResult {
    FS_CONTEXT
        .lock()
        .create_dir(path, NodePermission::default(), 0, 0)?;
    Ok(())
}

pub fn remove_dir(path: &str) -> AxResult {
    FS_CONTEXT.lock().remove_dir(path)?;
    Ok(())
}

pub fn remove_file(path: &str) -> AxResult {
    FS_CONTEXT.lock().remove_file(path)?;
    Ok(())
}

pub fn rename(old: &str, new: &str) -> AxResult {
    FS_CONTEXT.lock().rename(old, new)?;
    Ok(())
}

pub fn current_dir() -> AxResult<String> {
    FS_CONTEXT
        .lock()
        .current_dir()
        .absolute_path()
        .map(|path| path.to_string())
}

pub fn set_current_dir(path: &str) -> AxResult {
    let mut ctx = FS_CONTEXT.lock();
    let dir = ctx.resolve(path)?;
    ctx.set_current_dir(dir)?;
    Ok(())
}

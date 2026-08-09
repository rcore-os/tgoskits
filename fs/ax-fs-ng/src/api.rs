use alloc::string::{String, ToString};

use ax_errno::AxResult;
use axfs_ng_vfs::NodePermission;

use crate::{fops::FileAttr, highlevel::current_fs_context};

pub fn create_dir(path: &str) -> AxResult {
    current_fs_context()
        .lock()
        .create_dir(path, NodePermission::default(), 0, 0)?;
    Ok(())
}

pub fn remove_dir(path: &str) -> AxResult {
    current_fs_context().lock().remove_dir(path)?;
    Ok(())
}

pub fn remove_file(path: &str) -> AxResult {
    current_fs_context().lock().remove_file(path)?;
    Ok(())
}

pub fn rename(old: &str, new: &str) -> AxResult {
    current_fs_context().lock().rename(old, new)?;
    Ok(())
}

pub fn current_dir() -> AxResult<String> {
    current_fs_context()
        .lock()
        .current_dir()
        .absolute_path()
        .map(|path| path.to_string())
}

pub fn set_current_dir(path: &str) -> AxResult {
    let fs_context = current_fs_context();
    let mut ctx = fs_context.lock();
    let dir = ctx.resolve(path)?;
    ctx.set_current_dir(dir)?;
    Ok(())
}

/// Returns metadata for a path after resolving its final symbolic link.
pub fn metadata(path: &str) -> AxResult<FileAttr> {
    current_fs_context().lock().metadata(path)
}

/// Returns metadata for a path without resolving its final symbolic link.
pub fn symlink_metadata(path: &str) -> AxResult<FileAttr> {
    current_fs_context()
        .lock()
        .resolve_no_follow(path)?
        .metadata()
}

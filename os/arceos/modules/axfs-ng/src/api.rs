use alloc::string::{String, ToString};

use ax_errno::AxResult;
use axfs_ng_vfs::NodePermission;

use crate::highlevel::current_fs_context;

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
    let fs_context = current_fs_context();
    fs_context
        .lock()
        .with_namespace_operation(|namespace| namespace.current_dir().absolute_path())
        .map(|path| path.to_string())
}

pub fn set_current_dir(path: &str) -> AxResult {
    let fs_context = current_fs_context();
    let mut ctx = fs_context.lock();
    let dir = ctx.resolve_file_location(path)?;
    ctx.set_current_dir(dir)?;
    Ok(())
}

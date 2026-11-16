use alloc::string::{String, ToString};

use axfs_ng_vfs::{
    MutationCredentials, NodePermission, RenameOptions, VfsError, VfsResult, path::Path,
};

use crate::{
    fops::{DirHandle, FileAttr},
    fs_core::FsContext,
    highlevel::current_fs_context,
};

fn context_for_at(base: &FsContext, dir: Option<&DirHandle>, path: &str) -> VfsResult<FsContext> {
    if path.is_empty() {
        return Err(VfsError::NotFound);
    }
    match dir {
        Some(handle) if !Path::new(path).is_absolute() => {
            base.with_dirfd(handle.location().clone())
        }
        _ => Ok(base.clone()),
    }
}

pub(crate) fn with_at_context<R>(
    dir: Option<&DirHandle>,
    path: &str,
    f: impl FnOnce(&FsContext) -> VfsResult<R>,
) -> VfsResult<R> {
    let base = current_fs_context().lock().clone();
    let ctx = context_for_at(&base, dir, path)?;
    f(&ctx)
}

/// Creates a new, empty directory relative to `dir` (or cwd when `None`).
pub fn create_dir_at(dir: Option<&DirHandle>, path: &str) -> VfsResult {
    with_at_context(dir, path, |ctx| {
        ctx.create_dir(
            path,
            NodePermission::default(),
            0,
            0,
            &MutationCredentials::root(),
        )?;
        Ok(())
    })
}

pub fn create_dir(path: &str) -> VfsResult {
    create_dir_at(None, path)
}

/// Removes an empty directory relative to `dir` (or cwd when `None`).
pub fn remove_dir_at(dir: Option<&DirHandle>, path: &str) -> VfsResult {
    with_at_context(dir, path, |ctx| {
        ctx.remove_dir(path, &MutationCredentials::root())
    })
}

pub fn remove_dir(path: &str) -> VfsResult {
    remove_dir_at(None, path)
}

/// Removes a file relative to `dir` (or cwd when `None`).
pub fn remove_file_at(dir: Option<&DirHandle>, path: &str) -> VfsResult {
    with_at_context(dir, path, |ctx| {
        ctx.remove_file(path, &MutationCredentials::root())
    })
}

pub fn remove_file(path: &str) -> VfsResult {
    remove_file_at(None, path)
}

/// Renames a file or directory using independent directory handles for source
/// and destination relative paths.
pub fn rename_at(
    old_dir: Option<&DirHandle>,
    old_path: &str,
    new_dir: Option<&DirHandle>,
    new_path: &str,
) -> VfsResult {
    let base = current_fs_context().lock().clone();
    let creds = MutationCredentials::root();
    let src_ctx = context_for_at(&base, old_dir, old_path)?;
    let (src_dir, src_name, src_searched) = src_ctx
        .resolve_parent_with_search_checked(Path::new(old_path), |directory| {
            src_ctx.check_search_path(directory, src_ctx.permission_boundary(), &creds)
        })?;
    let dst_ctx = context_for_at(&base, new_dir, new_path)?;
    let (dst_dir, dst_name, dst_searched) = dst_ctx
        .resolve_parent_with_search_checked(Path::new(new_path), |directory| {
            dst_ctx.check_search_path(directory, dst_ctx.permission_boundary(), &creds)
        })?;
    base.rename_locations_with_boundaries_and_search(
        (&src_dir, &src_name),
        (&dst_dir, &dst_name),
        RenameOptions::REPLACE,
        (src_ctx.permission_boundary(), dst_ctx.permission_boundary()),
        (&src_searched, &dst_searched),
        &creds,
    )
}

pub fn rename(old: &str, new: &str) -> VfsResult {
    rename_at(None, old, None, new)
}

pub fn current_dir() -> VfsResult<String> {
    current_fs_context()
        .lock()
        .current_dir()
        .absolute_path()
        .map(|path| path.to_string())
}

pub fn set_current_dir(path: &str) -> VfsResult {
    let fs_context = current_fs_context();
    let mut ctx = fs_context.lock();
    let dir = ctx.resolve(path)?;
    ctx.set_current_dir(dir)?;
    Ok(())
}

/// Returns metadata for the current working directory.
pub fn current_dir_metadata() -> VfsResult<FileAttr> {
    current_fs_context().lock().current_dir().metadata()
}

/// Returns metadata for a path resolved relative to `dir` (or cwd when `None`).
pub fn metadata_at(
    dir: Option<&DirHandle>,
    path: &str,
    follow_symlinks: bool,
) -> VfsResult<FileAttr> {
    with_at_context(dir, path, |ctx| {
        if follow_symlinks {
            ctx.metadata(path)
        } else {
            ctx.resolve_no_follow(path)?.metadata()
        }
    })
}

/// Returns metadata for a path after resolving its final symbolic link.
pub fn metadata(path: &str) -> VfsResult<FileAttr> {
    metadata_at(None, path, true)
}

/// Returns metadata for a path without resolving its final symbolic link.
pub fn symlink_metadata(path: &str) -> VfsResult<FileAttr> {
    metadata_at(None, path, false)
}

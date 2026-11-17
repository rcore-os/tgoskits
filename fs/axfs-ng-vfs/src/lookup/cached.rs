use alloc::sync::Arc;

use super::{LookupPolicy, LookupRestrictions, LookupScope};
use crate::{
    Location, MutationCredentials, NodePermission, NodeType, PathHandle, PathObservation, VfsError,
    VfsResult,
};

/// Already owned task inputs for one cache-only path operation.
///
/// The caller selects root, start and credentials together before constructing
/// this context. Both paths retain their original counted mount uses.
#[derive(Debug)]
pub struct CachedLookupContext<'a> {
    pub root: &'a PathHandle,
    pub start: &'a PathHandle,
    pub credentials: MutationCredentials<'a>,
    pub policy: LookupPolicy,
}

/// Final-component requirements independent of syscall flag numbers.
#[derive(Clone, Copy, Debug)]
pub struct CachedLookupOptions {
    pub follow_final_link: bool,
    pub require_directory: bool,
}

/// A closed cache-only observation with no ordinary backend escape hatch.
///
/// Only explicit cache capabilities and try-locks are used during traversal.
/// Missing cache state or an invalidated observation returns `WouldBlock`.
/// `resolve` releases an owned path only after the complete observation passes.
#[derive(Debug)]
pub struct CachedLookup<'a> {
    context: CachedLookupContext<'a>,
    observation: PathObservation,
}

impl<'a> CachedLookup<'a> {
    pub fn new(context: CachedLookupContext<'a>) -> VfsResult<Self> {
        Ok(Self {
            context,
            observation: PathObservation::capture()?,
        })
    }

    pub fn resolve(self, path: &str, options: CachedLookupOptions) -> VfsResult<PathHandle> {
        if path.is_empty() {
            return Err(VfsError::NotFound);
        }
        if self.context.policy.scope == LookupScope::Beneath && path.starts_with('/') {
            return Err(VfsError::CrossesDevices);
        }
        let result = self.walk(path, options);
        // Negative and permission results require a coherent observation too.
        self.observation.validate()?;
        result
    }

    fn root(&self) -> &PathHandle {
        if self.context.policy.scope == LookupScope::ProcessRoot {
            self.context.root
        } else {
            self.context.start
        }
    }

    fn walk(&self, path: &str, options: CachedLookupOptions) -> VfsResult<PathHandle> {
        let mut current = if path.starts_with('/') {
            self.root().clone()
        } else {
            self.context.start.clone()
        };
        let directory = options.require_directory || path.ends_with('/');
        let mut components = path.split('/').filter(|part| !part.is_empty()).peekable();
        while let Some(component) = components.next() {
            self.observation.validate()?;
            self.check_search(&current)?;
            match component {
                "." => {}
                ".." => current = self.ascend(current)?,
                name => {
                    crate::path::verify_entry_name(name)?;
                    let entry = current.location().entry().as_dir()?.cached_child(name)?;
                    let child = Location::new(current.location().mountpoint().clone(), entry);
                    current = self.follow_mounts(self.bind(&current, child)?)?;
                    if current.location().node_type() == NodeType::Symlink
                        && (components.peek().is_some() || options.follow_final_link || directory)
                    {
                        if self
                            .context
                            .policy
                            .restrictions
                            .contains(LookupRestrictions::NO_SYMLINKS)
                        {
                            return Err(VfsError::FilesystemLoop);
                        }
                        // No normal LinkOps callback is permitted here. Link
                        // producers need their own audited cache capability.
                        return Err(VfsError::WouldBlock);
                    }
                }
            }
        }
        if directory && !current.location().is_dir() {
            return Err(VfsError::NotADirectory);
        }
        Ok(current)
    }

    fn check_search(&self, path: &PathHandle) -> VfsResult<()> {
        let entry = path.location().entry();
        entry.as_dir()?;
        self.context
            .credentials
            .check_permission(&entry.cached_metadata()?, NodePermission::OTHER_EXEC)
    }

    fn bind(&self, from: &PathHandle, location: Location) -> VfsResult<PathHandle> {
        if Arc::ptr_eq(from.location().mountpoint(), location.mountpoint()) {
            from.rebind(location)
        } else if self
            .context
            .policy
            .restrictions
            .contains(LookupRestrictions::NO_XDEV)
        {
            Err(VfsError::CrossesDevices)
        } else {
            PathHandle::acquire_cached(location)
        }
    }

    fn follow_mounts(&self, mut path: PathHandle) -> VfsResult<PathHandle> {
        while let Some(covered) = path.location().cached_cover()? {
            self.observation.validate()?;
            path = self.bind(&path, covered)?;
        }
        Ok(path)
    }

    fn ascend(&self, mut path: PathHandle) -> VfsResult<PathHandle> {
        loop {
            if path.location().ptr_eq(self.root().location()) {
                return if self.context.policy.scope == LookupScope::Beneath {
                    Err(VfsError::CrossesDevices)
                } else {
                    Ok(path)
                };
            }
            self.observation.validate()?;
            if path.location().is_root_of_mount() {
                match path.location().cached_covered_location()? {
                    Some(covered) => path = self.bind(&path, covered)?,
                    None => return Ok(path),
                }
            } else {
                return match path.location().entry().cached_parent()? {
                    Some(parent) => {
                        let location = Location::new(path.location().mountpoint().clone(), parent);
                        self.bind(&path, location)
                    }
                    None => Ok(path),
                };
            }
        }
    }
}

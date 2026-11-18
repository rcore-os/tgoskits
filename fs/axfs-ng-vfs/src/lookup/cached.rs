use alloc::sync::Arc;

use smallvec::SmallVec;

use super::{LookupPolicy, LookupRestrictions, LookupScope, SYMLINKS_MAX};
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

enum PathText<'a> {
    Input(&'a str),
    Link(Arc<str>),
}

impl PathText<'_> {
    fn as_str(&self) -> &str {
        match self {
            Self::Input(text) => text,
            Self::Link(text) => text,
        }
    }
}

struct PathFrame<'a> {
    text: PathText<'a>,
    offset: usize,
}

impl PathFrame<'_> {
    fn next_component(&mut self) -> Option<(&str, bool)> {
        let text = self.text.as_str();
        let bytes = text.as_bytes();
        while self.offset < bytes.len() && bytes[self.offset] == b'/' {
            self.offset += 1;
        }
        let start = self.offset;
        while self.offset < bytes.len() && bytes[self.offset] != b'/' {
            self.offset += 1;
        }
        if start == self.offset {
            return None;
        }
        let more = bytes[self.offset..].iter().any(|byte| *byte != b'/');
        Some((&text[start..self.offset], more))
    }
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
        let mut trailing_directory = path.ends_with('/');
        let mut pending = SmallVec::<[PathFrame<'_>; 1]>::new();
        pending.push(PathFrame {
            text: PathText::Input(path),
            offset: 0,
        });
        let mut links = 0;
        while let Some(mut frame) = pending.pop() {
            let Some((component, more_in_frame)) = frame.next_component() else {
                continue;
            };
            let more = more_in_frame || !pending.is_empty();
            let mut target = None;
            self.observation.validate()?;
            self.check_search(&current)?;
            match component {
                "." => {}
                ".." => current = self.ascend(current)?,
                name => {
                    crate::path::verify_entry_name(name)?;
                    let entry = current.location().entry().as_dir()?.cached_child(name)?;
                    let child = Location::new(current.location().mountpoint().clone(), entry);
                    let child = self.follow_mounts(self.bind(&current, child)?)?;
                    if child.location().node_type() == NodeType::Symlink
                        && (more || options.follow_final_link || trailing_directory)
                    {
                        if self
                            .context
                            .policy
                            .restrictions
                            .contains(LookupRestrictions::NO_SYMLINKS)
                        {
                            return Err(VfsError::FilesystemLoop);
                        }
                        if links == SYMLINKS_MAX {
                            return Err(VfsError::FilesystemLoop);
                        }
                        links += 1;
                        target = Some(child.location().entry().cached_link_target()?);
                    } else {
                        current = child;
                    }
                }
            }
            if more_in_frame {
                pending.push(frame);
            }
            if let Some(target) = target {
                if target.is_empty() {
                    return Err(VfsError::NotFound);
                }
                if target.starts_with('/') {
                    current = self.jump_root(&current)?;
                }
                if !more && target.ends_with('/') {
                    trailing_directory = true;
                }
                pending.push(PathFrame {
                    text: PathText::Link(target),
                    offset: 0,
                });
            }
        }
        if (options.require_directory || trailing_directory) && !current.location().is_dir() {
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

    fn jump_root(&self, current: &PathHandle) -> VfsResult<PathHandle> {
        if self.context.policy.scope == LookupScope::Beneath {
            return Err(VfsError::CrossesDevices);
        }
        if self
            .context
            .policy
            .restrictions
            .contains(LookupRestrictions::NO_XDEV)
            && !Arc::ptr_eq(
                current.location().mountpoint(),
                self.root().location().mountpoint(),
            )
        {
            return Err(VfsError::CrossesDevices);
        }
        Ok(self.root().clone())
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
                    // Linux's RCU dotdot refuses this escape before entering
                    // ref-walk; CACHED cannot unlazy to report ordinary EXDEV.
                    Err(VfsError::WouldBlock)
                } else {
                    Ok(path)
                };
            }
            self.observation.validate()?;
            if path.location().is_root_of_mount() {
                match path.location().cached_covered_location()? {
                    Some(covered) => {
                        if self
                            .context
                            .policy
                            .restrictions
                            .contains(LookupRestrictions::NO_XDEV)
                        {
                            return Err(VfsError::WouldBlock);
                        }
                        path = self.bind(&path, covered)?;
                    }
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

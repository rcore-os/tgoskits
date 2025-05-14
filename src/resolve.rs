use lock_api::RawMutex;

use crate::{path::{Component, Path}, Location, VfsError, VfsResult};

pub struct FsResolver<M> {
    root_dir: Location<M>,
    current_dir: Location<M>,
}
impl<M> Clone for FsResolver<M> {
    fn clone(&self) -> Self {
        Self {
            root_dir: self.root_dir.clone(),
            current_dir: self.current_dir.clone(),
        }
    }
}
impl<M: RawMutex> FsResolver<M> {
    pub fn new(root_dir: Location<M>) -> Self {
        Self {
            root_dir: root_dir.clone(),
            current_dir: root_dir,
        }
    }

    pub fn root_dir(&self) -> &Location<M> {
        &self.root_dir
    }
    pub fn current_dir(&self) -> &Location<M> {
        &self.current_dir
    }

    pub fn set_current_dir(&mut self, current_dir: Location<M>) -> VfsResult<()> {
        current_dir.check_is_dir()?;
        self.current_dir = current_dir;
        Ok(())
    }

    pub fn with_current_dir(&self, current_dir: Location<M>) -> VfsResult<Self> {
        current_dir.check_is_dir()?;
        Ok(Self {
            root_dir: self.root_dir.clone(),
            current_dir,
        })
    }

    fn resolve_inner<'a>(&self, path: &'a Path) -> VfsResult<(Location<M>, Option<&'a str>)> {
        let mut dir = self.current_dir.clone();

        let entry_name = path.file_name();
        let mut components = path.components();
        if entry_name.is_some() {
            components.next_back();
        }
        for comp in components {
            match comp {
                Component::CurDir => {}
                Component::ParentDir => {
                    dir = dir.parent().unwrap_or_else(|| self.root_dir.clone());
                }
                Component::RootDir => {
                    dir = self.root_dir.clone();
                }
                Component::Normal(name) => {
                    dir = dir.lookup(name)?;
                }
            }
        }
        dir.check_is_dir()?;
        Ok((dir, entry_name))
    }

    /// Taking current node as root directory, resolves a path starting from
    /// `current_dir`.
    pub fn resolve(&self, path: impl AsRef<Path>) -> VfsResult<Location<M>> {
        let (dir, name) = self.resolve_inner(path.as_ref())?;
        match name {
            Some(name) => dir.lookup(name),
            None => Ok(dir),
        }
    }

    /// Taking current node as root directory, resolves a path starting from
    /// `current_dir`.
    ///
    /// Returns `(parent_dir, entry_name)`, where `entry_name` is the name of
    /// the entry.
    pub fn resolve_parent<'a>(&self, path: &'a Path) -> VfsResult<(Location<M>, Cow<'a, str>)> {
        let (dir, name) = self.resolve_inner(path)?;
        if let Some(name) = name {
            Ok((dir, Cow::Borrowed(name)))
        } else if let Some(parent) = dir.parent() {
            Ok((parent, Cow::Owned(dir.name().to_owned())))
        } else {
            Err(VfsError::EINVAL)
        }
    }

    /// Resolves a path starting from `current_dir`, returning the parent
    /// directory and the name of the entry.
    ///
    /// This function requires that the entry does not exist and the parent
    /// exists. Note that, it does not perform an actual check to ensure the
    /// entry's non-existence. It simply raises an error if the entry name is
    /// not present in the path.
    pub fn resolve_nonexistent<'a>(&self, path: &'a Path) -> VfsResult<(Location<M>, &'a str)> {
        let (dir, name) = self.resolve_inner(path)?;
        if let Some(name) = name {
            Ok((dir, name))
        } else {
            Err(VfsError::EEXIST)
        }
    }
}

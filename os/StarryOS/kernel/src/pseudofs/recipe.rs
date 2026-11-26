//! Filesystem-free construction recipes, bound only for an active operation.

use alloc::sync::Arc;

use axfs_ng_vfs::{DirNodeOps, FileNodeOps, VfsError, VfsResult, WeakDirEntry};

use super::{SimpleFsContext, SimpleFsOwner};

pub(super) trait DirRecipe: Send + Sync + 'static {
    fn materialize(
        self: Arc<Self>,
        owner: SimpleFsOwner,
        this: WeakDirEntry,
    ) -> Arc<dyn DirNodeOps>;
}

pub(super) trait FileRecipe: Send + Sync + 'static {
    fn materialize(self: Arc<Self>, owner: SimpleFsOwner) -> Arc<dyn FileNodeOps>;
}

/// Shared entity recipe that cannot be invoked without its original receiver.
#[derive(Clone)]
pub struct DirMaker {
    context: SimpleFsContext,
    recipe: Arc<dyn DirRecipe>,
}

impl DirMaker {
    pub(super) fn new(context: SimpleFsContext, recipe: impl DirRecipe) -> Self {
        Self {
            context,
            recipe: Arc::new(recipe),
        }
    }

    pub(super) fn check_context(&self, context: &SimpleFsContext) -> VfsResult<()> {
        if self.context.same_instance(context) {
            Ok(())
        } else {
            Err(VfsError::BadState)
        }
    }

    pub fn prepare(&self, owner: SimpleFsOwner) -> VfsResult<PreparedDir> {
        self.check_context(&owner.context())?;
        Ok(PreparedDir {
            owner,
            recipe: self.recipe.clone(),
        })
    }
}

/// A checked binding, consumed exactly once by directory-entry assembly.
pub struct PreparedDir {
    owner: SimpleFsOwner,
    recipe: Arc<dyn DirRecipe>,
}

impl PreparedDir {
    pub fn materialize(self, this: WeakDirEntry) -> Arc<dyn DirNodeOps> {
        self.recipe.materialize(self.owner, this)
    }
}

/// Shared file backing, without an active filesystem receiver.
#[derive(Clone)]
pub struct FileMaker {
    context: SimpleFsContext,
    recipe: Arc<dyn FileRecipe>,
}

impl FileMaker {
    pub(super) fn new(context: SimpleFsContext, recipe: Arc<dyn FileRecipe>) -> Self {
        Self { context, recipe }
    }

    pub fn prepare(&self, owner: SimpleFsOwner) -> VfsResult<PreparedFile> {
        if !self.context.same_instance(&owner.context()) {
            return Err(VfsError::BadState);
        }
        Ok(PreparedFile {
            owner,
            recipe: self.recipe.clone(),
        })
    }
}

/// A checked binding, consumed exactly once by file-wrapper assembly.
pub struct PreparedFile {
    owner: SimpleFsOwner,
    recipe: Arc<dyn FileRecipe>,
}

impl PreparedFile {
    pub fn materialize(self) -> Arc<dyn FileNodeOps> {
        self.recipe.materialize(self.owner)
    }
}

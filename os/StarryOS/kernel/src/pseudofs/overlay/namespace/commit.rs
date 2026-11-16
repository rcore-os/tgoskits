//! Accepted merged commands descend through the original operation context.

use axfs_ng_vfs::{
    AuthorizedCreate, AuthorizedLink, AuthorizedRename, AuthorizedUnlink, CreateAttributes,
    CreateContent, CreateEffect, CreateIntent, NodePermission, NodeType,
};

use super::*;
use crate::pseudofs::overlay::{OPAQUE_MARKER_NAME, WHITEOUT_DEVICE};

impl NamespaceAttempt<'_> {
    fn upper(&self, parent: usize) -> VfsResult<Location> {
        let inode = self.fs.inode_for(self.parents[parent].entry())?;
        let upper = inode.upper().ok_or(VfsError::BadState)?;
        self.context.retired().retire(upper.clone());
        Ok(upper)
    }

    fn upper_child(
        &mut self,
        parent: usize,
        name: &str,
    ) -> VfsResult<Option<(Location, Metadata)>> {
        let inode = self.fs.inode_for(self.parents[parent].entry())?;
        let Some(upper) = inode.upper() else {
            return Ok(None);
        };
        self.context.retired().retire(upper.clone());
        self.read(self.layers.child_snapshot(&upper, name))
    }

    fn lower_exists(&mut self, parent: usize, name: &str) -> VfsResult<bool> {
        let inode = self.fs.inode_for(self.parents[parent].entry())?;
        if let Some(upper) = inode.upper() {
            self.context.retired().retire(upper.clone());
            if self.read(self.layers.opaque(&upper))? {
                return Ok(false);
            }
        }
        Ok(!self
            .read(self.layers.lower_children(&inode.lower, name, false))?
            .is_empty())
    }

    fn whiteout(metadata: &Metadata) -> bool {
        metadata.node_type == NodeType::CharacterDevice && metadata.rdev == WHITEOUT_DEVICE
    }

    fn whiteout_slot(&mut self, parent: usize, name: &str) -> VfsResult<Option<Location>> {
        Ok(self
            .upper_child(parent, name)?
            .filter(|(_, metadata)| Self::whiteout(metadata))
            .map(|(location, _)| location))
    }

    fn remove_native(
        &mut self,
        inode: &Arc<OverlayInode>,
        name: &str,
        expected: Option<&Location>,
        directory: bool,
    ) -> VfsResult<()> {
        let plan = self.plan(inode)?;
        let prepared = plan.unlink(name)?;
        let credentials = self.fs.credentials();
        let result = match self
            .context
            .unlink_backing(&prepared, directory, &credentials)?
        {
            NamespaceStart::Observed(observed) => {
                let matches = match expected {
                    Some(expected) => NamespaceNode {
                        inode: expected.inode(),
                        node_type: expected.node_type(),
                        identity: expected.entry().identity().cloned(),
                    }
                    .matches(&observed.target().entry),
                    None => {
                        name == OPAQUE_MARKER_NAME || Self::whiteout(&observed.target().metadata)
                    }
                };
                if !matches {
                    self.child(observed.cancel())?;
                    return Err(VfsError::BadState);
                }
                observed.resume()
            }
            NamespaceStart::Complete(result) => result,
        };
        self.child(result)
    }

    fn prepare_whiteout(
        &self,
        parent: usize,
        name: &str,
        whiteout: &Option<Location>,
    ) -> VfsResult<bool> {
        if whiteout.is_none() {
            return Ok(false);
        }
        self.prepare_unlink(&self.fs.inode_for(self.parents[parent].entry())?, name)
    }

    fn remove_whiteout(
        &mut self,
        parent: usize,
        name: &str,
        whiteout: Option<Location>,
    ) -> VfsResult<()> {
        if let Some(whiteout) = whiteout {
            let inode = self.fs.inode_for(self.parents[parent].entry())?;
            self.remove_native(&inode, name, Some(&whiteout), false)?;
        }
        Ok(())
    }

    fn make_whiteout(&mut self, parent: usize, name: &str) -> VfsResult<()> {
        let upper = self.upper(parent)?;
        let credentials = self.fs.credentials();
        let result = self.context.create(
            &upper,
            name,
            CreateAttributes {
                content: CreateContent::Node(NodeType::CharacterDevice, WHITEOUT_DEVICE),
                permission: NodePermission::empty(),
            },
            CreateIntent::Exclusive,
            &credentials,
        )?;
        self.child(result)
    }

    fn opaque(&mut self, upper: &Location) -> VfsResult<()> {
        if self.read(self.layers.opaque(upper))? {
            return Ok(());
        }
        let credentials = self.fs.credentials();
        let result = self.context.create(
            upper,
            OPAQUE_MARKER_NAME,
            CreateAttributes {
                content: CreateContent::Node(NodeType::RegularFile, Default::default()),
                permission: NodePermission::from_bits_truncate(0o600),
            },
            CreateIntent::Exclusive,
            &credentials,
        )?;
        self.child(result)
    }

    fn prune_plan(
        &mut self,
        entry: &DirEntry,
    ) -> VfsResult<(Arc<OverlayInode>, Vec<alloc::string::String>, bool)> {
        let inode = self.fs.inode_for(entry)?;
        let names = match inode.upper() {
            Some(upper) => {
                self.context.retired().retire(upper.clone());
                self.read(self.layers.raw_names(&upper))?
            }
            None => Vec::new(),
        };
        let mut pending = false;
        for name in &names {
            pending |= self.prepare_unlink(&inode, name)?;
        }
        Ok((inode, names, pending))
    }

    fn prune(
        &mut self,
        inode: &Arc<OverlayInode>,
        names: &[alloc::string::String],
    ) -> VfsResult<()> {
        for name in names {
            self.remove_native(inode, name, None, false)?;
        }
        Ok(())
    }

    pub(super) fn create(
        &mut self,
        authorized: &AuthorizedCreate<'_>,
    ) -> VfsResult<NamespaceOutcome<NamespaceEffect>> {
        let request = authorized.request();
        let forest = self.forest(None)?;
        let whiteout = self.whiteout_slot(0, request.name())?;
        let pending = self.prepare_whiteout(0, request.name(), &whiteout)?;
        if forest.pending() || pending {
            return Ok(NamespaceOutcome::retry());
        }
        self.publish(&forest)?;
        let was_whiteout = whiteout.is_some();
        self.remove_whiteout(0, request.name(), whiteout)?;
        let upper = self.upper(0)?;
        // Linux overrides fsuid/fsgid for creation while retaining the saved
        // mounter's groups and capabilities. No root restoration is substituted.
        let mut credentials = self.fs.credentials();
        (credentials.fsuid, credentials.fsgid) = authorized.owner();
        let attributes = CreateAttributes {
            content: request.attributes().content,
            permission: authorized.permission(),
        };
        let result = self.context.create(
            &upper,
            request.name(),
            attributes,
            CreateIntent::Exclusive,
            &credentials,
        )?;
        let selected = match result.observation() {
            Some(NamespaceEffect::Create(effect)) if effect.created => Some(Location::new(
                upper.mountpoint().clone(),
                effect.selected.entry.clone(),
            )),
            _ => None,
        };
        if let Some(selected) = &selected {
            self.context.retired().retire(selected.clone());
        }
        self.child(result)?;
        let selected = selected.ok_or(VfsError::BadState)?;
        if was_whiteout && selected.is_dir() {
            self.opaque(&selected)?;
        }
        let selected = self.lookup(0, request.name())?.ok_or(VfsError::BadState)?;
        Ok(NamespaceOutcome::applied(
            NamespaceEffect::Create(CreateEffect {
                selected: selected.selected(),
                created: true,
            }),
            Ok(()),
        ))
    }

    pub(super) fn link(
        &mut self,
        authorized: &AuthorizedLink<'_>,
    ) -> VfsResult<NamespaceOutcome<NamespaceEffect>> {
        let request = authorized.request();
        let forest = self.forest(Some(request.source()))?;
        let whiteout = self.whiteout_slot(0, request.name())?;
        let pending = self.prepare_whiteout(0, request.name(), &whiteout)?;
        if forest.pending() || pending {
            return Ok(NamespaceOutcome::retry());
        }
        self.publish(&forest)?;
        self.remove_whiteout(0, request.name(), whiteout)?;
        let upper = self.upper(0)?;
        let source = self
            .fs
            .inode_for(request.source())?
            .upper()
            .ok_or(VfsError::BadState)?;
        self.context.retired().retire(source.clone());
        let credentials = self.fs.credentials();
        let result = self
            .context
            .link(&upper, request.name(), &source, &credentials)?;
        self.child(result)?;
        let selected = self.lookup(0, request.name())?.ok_or(VfsError::BadState)?;
        Ok(NamespaceOutcome::applied(
            NamespaceEffect::Link(selected.selected()),
            Ok(()),
        ))
    }

    pub(super) fn unlink(
        &mut self,
        authorized: &AuthorizedUnlink<'_>,
    ) -> VfsResult<NamespaceOutcome<NamespaceEffect>> {
        let request = authorized.request();
        let forest = self.forest(None)?;
        let inode = self.fs.inode_for(&authorized.target().entry)?;
        self.context.retired().retire(inode.clone());
        let upper = inode.upper();
        if let Some(upper) = &upper {
            self.context.retired().retire(upper.clone());
        }
        let lower = self.lower_exists(0, request.name())?;
        let parent = self.fs.inode_for(self.parents[0].entry())?;
        let mut pending = if upper.is_some() {
            self.prepare_unlink(&parent, request.name())?
        } else {
            false
        };
        let pruning = if authorized.target().entry.is_dir() {
            Some(self.prune_plan(&authorized.target().entry)?)
        } else {
            None
        };
        if let Some((_, _, more)) = &pruning {
            pending |= more;
        }
        if forest.pending() || pending {
            return Ok(NamespaceOutcome::retry());
        }
        self.publish(&forest)?;
        if let Some((inode, names, _)) = pruning {
            self.prune(&inode, &names)?;
        }
        if let Some(upper) = upper {
            self.remove_native(&parent, request.name(), Some(&upper), request.directory())?;
        }
        if lower {
            self.make_whiteout(0, request.name())?;
        }
        Ok(NamespaceOutcome::applied(
            NamespaceEffect::Unlink(authorized.target().selected()),
            Ok(()),
        ))
    }

    pub(super) fn rename(
        &mut self,
        authorized: &AuthorizedRename<'_>,
    ) -> VfsResult<NamespaceOutcome<NamespaceEffect>> {
        let request = authorized.request();
        let source = &authorized.observation().source;
        let inode = self.fs.inode_for(&source.entry)?;
        if source.entry.is_dir() && !inode.lower.is_empty() {
            return Err(VfsError::CrossesDevices);
        }
        let lower = self.lower_exists(0, request.source_name())?;
        let forest = self.forest(Some(&source.entry))?;
        let whiteout = self.whiteout_slot(1, request.destination_name())?;
        let mut pending = self.prepare_whiteout(1, request.destination_name(), &whiteout)?;
        let pruning = match &authorized.observation().target {
            Some(target) if target.entry.is_dir() => Some(self.prune_plan(&target.entry)?),
            _ => None,
        };
        if let Some((_, _, more)) = &pruning {
            pending |= more;
        }
        if forest.pending() || pending {
            return Ok(NamespaceOutcome::retry());
        }
        self.publish(&forest)?;
        self.remove_whiteout(1, request.destination_name(), whiteout)?;
        if let Some((inode, names, _)) = pruning {
            self.prune(&inode, &names)?;
        }
        let source_parent = self.upper(0)?;
        let destination_parent = self.upper(1)?;
        let credentials = self.fs.credentials();
        let result = self.context.rename(
            (&source_parent, request.source_name()),
            (&destination_parent, request.destination_name()),
            request.options(),
            &credentials,
        )?;
        self.child(result)?;
        if lower {
            self.make_whiteout(0, request.source_name())?;
        }
        if source.entry.is_dir() {
            let upper = inode.upper().ok_or(VfsError::BadState)?;
            self.context.retired().retire(upper.clone());
            self.opaque(&upper)?;
        }
        Ok(NamespaceOutcome::applied(
            NamespaceEffect::Rename(authorized.effect()),
            Ok(()),
        ))
    }
}

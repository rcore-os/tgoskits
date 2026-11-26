//! Reference preparation reusable only by one actual composite command.

use alloc::sync::Arc;

use super::{NamespaceOperationContext, registration::Reservation};
use crate::{
    Location, MutationCredentials, NamespaceStart, PreparedUnlink, PreparedWrite, VfsError,
    VfsResult,
};

/// Owned provenance for lock-external reference preparation. It retains the
/// receiver and one command identity, never an admission, driver or authority.
#[derive(Clone)]
pub struct BackingUnlinkOrigin {
    command: Arc<()>,
    receiver: Location,
}

/// One fully prepared native parent/name, reusable through this command's
/// fresh attempts. Ordinary PreparedUnlink remains non-Clone; other commands
/// must prepare again. No public method exposes a duplicable native template.
pub struct PreparedBackingUnlink {
    origin: BackingUnlinkOrigin,
    native: PreparedUnlink,
}

impl BackingUnlinkOrigin {
    pub(super) fn new(command: Arc<()>, receiver: Location) -> Self {
        Self { command, receiver }
    }

    fn check_backing(&self, parent: &Location) -> VfsResult<()> {
        let source = self.receiver.filesystem();
        let destination = parent.filesystem();
        if source.stack_depth() > 2
            || destination.stack_depth() >= source.stack_depth()
            || !source.write_backing_roots().iter().any(|root| {
                core::ptr::addr_eq(root.filesystem(), destination)
                    && Arc::ptr_eq(root.mountpoint(), parent.mountpoint())
            })
        {
            return Err(VfsError::BadState);
        }
        Ok(())
    }

    /// Call outside all ancestor gates/admissions. Preparation releases cached
    /// references and performs any required writeback; it changes no name.
    pub fn prepare(
        &self,
        write: &PreparedWrite,
        parent: &Location,
        name: &str,
    ) -> VfsResult<PreparedBackingUnlink> {
        write
            .write_for(&self.receiver)?
            .check_location(&self.receiver)?;
        write.write_for(parent)?.check_location(parent)?;
        self.check_backing(parent)?;
        Ok(PreparedBackingUnlink {
            origin: self.clone(),
            native: parent.prepare_unlink(name)?,
        })
    }
}

impl PreparedBackingUnlink {
    /// The actual prepared native parent, independent of mutable pathname text.
    pub fn parent(&self) -> Location {
        self.native.parent_owner()
    }

    pub fn name(&self) -> &str {
        self.native.name()
    }
}

impl<'op> NamespaceOperationContext<'op> {
    /// Starts a fresh backing observation with this command's prepared native
    /// references. Every resume still uses the fixed current native authorizer.
    pub fn unlink_backing<'a>(
        &'a self,
        prepared: &PreparedBackingUnlink,
        directory: bool,
        credentials: &MutationCredentials<'a>,
    ) -> VfsResult<NamespaceStart<'a>> {
        let parent = prepared.native.parent_owner();
        self.retired().retire(parent.clone());
        self.retired().retire(prepared.origin.receiver.clone());
        if !self.command.has_identity(&prepared.origin.command) {
            return Err(VfsError::BadState);
        }
        self.admission(&prepared.origin.receiver)?;
        self.write_for(&prepared.origin.receiver)?
            .check_location(&prepared.origin.receiver)?;
        prepared.origin.check_backing(&parent)?;
        let native = PreparedUnlink::new(parent, prepared.native.name().into());
        let reservation = Reservation::child(self.children)?;
        crate::node::start_unlink(self, native, directory, credentials, reservation)
    }
}

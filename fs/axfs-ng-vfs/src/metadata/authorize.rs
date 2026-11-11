//! Flat, non-idmapped Linux setattr authorization, against the locked inode.

use core::time::Duration;

use super::{AuthorizedMetadataChange, MetadataChange, MetadataRequest, TimeChange};
use crate::{Metadata, NodePermission, NodeType, VfsError, VfsResult};

impl MetadataRequest<'_> {
    pub(super) fn authorize<'a>(
        &'a self,
        current: &Metadata,
        now: Duration,
    ) -> VfsResult<AuthorizedMetadataChange<'a>> {
        let credentials = self.credentials;
        let owner = credentials.fsuid == current.uid;
        let owner_or_capable = || {
            if owner || credentials.cap_fowner {
                Ok(())
            } else {
                Err(VfsError::OperationNotPermitted)
            }
        };
        let mut authorized = AuthorizedMetadataChange {
            _request: self,
            mode: None,
            uid: None,
            gid: None,
            atime: None,
            mtime: None,
            ctime: now,
        };
        match self.change {
            MetadataChange::Chmod(mode) => {
                // notify_change rejects explicit ATTR_MODE before setattr_prepare.
                // This is deliberately NOT applied to chown's derived ATTR_MODE.
                if current.node_type == NodeType::Symlink {
                    return Err(VfsError::OperationNotSupported);
                }
                authorized.mode = Some(mode);
            }
            MetadataChange::Chown { uid, gid } => {
                // setattr_prepare checks UID, then GID, then any derived MODE.
                if let Some(uid) = uid
                    && !credentials.cap_chown
                    && !(owner && uid == current.uid)
                {
                    return Err(VfsError::OperationNotPermitted);
                }
                if let Some(gid) = gid
                    && !credentials.cap_chown
                    && !(owner && (gid == current.gid || credentials.in_group(gid)))
                {
                    return Err(VfsError::OperationNotPermitted);
                }
                authorized.uid = uid;
                authorized.gid = gid;
                if current.node_type != NodeType::Directory {
                    let mut mode = current.mode;
                    // chown_common has no CAP_FSETID exemption for SUID.
                    mode.remove(NodePermission::SET_UID);
                    if mode.contains(NodePermission::SET_GID)
                        && (mode.contains(NodePermission::GROUP_EXEC)
                            || !(credentials.in_group(current.gid) || credentials.cap_fsetid))
                    {
                        mode.remove(NodePermission::SET_GID);
                    }
                    // notify_change adds ATTR_MODE only when a bit is killed.
                    if mode.bits() != current.mode.bits() {
                        authorized.mode = Some(mode);
                    }
                }
            }
            MetadataChange::Times { atime, mtime } => {
                if atime == TimeChange::Now && mtime == TimeChange::Now {
                    if !owner && !credentials.cap_fowner {
                        credentials.check_permission(current, NodePermission::OTHER_WRITE)?;
                    }
                } else {
                    owner_or_capable()?;
                }
                let resolve = |time| match time {
                    TimeChange::Keep => None,
                    TimeChange::Now => Some(now),
                    TimeChange::Set(value) => Some(value),
                };
                authorized.atime = resolve(atime);
                authorized.mtime = resolve(mtime);
            }
        }
        if let Some(mode) = &mut authorized.mode {
            owner_or_capable()?;
            // With ATTR_GID, setattr_prepare uses the requested group here.
            let gid = authorized.gid.unwrap_or(current.gid);
            if !credentials.in_group(gid) && !credentials.cap_fsetid {
                mode.remove(NodePermission::SET_GID);
            }
        }
        Ok(authorized)
    }
}

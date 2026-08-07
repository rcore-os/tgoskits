//! Metadata update request types.

use crate::disknode::Ext4TimeSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ext4MetadataReason {
    Create,
    Read,
    Write,
    Truncate,
    Chmod,
    Chown,
    Utimens,
    LinkCount,
    ParentDir,
    Flags,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ext4ModeUpdate {
    Replace(u16),
    Chmod(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Ext4DtimeUpdate {
    #[default]
    Keep,
    Clear,
    SetNow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ext4InodeMetadataUpdate {
    pub reason: Ext4MetadataReason,
    pub mode: Option<Ext4ModeUpdate>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub projid: Option<u32>,
    pub atime: Option<Ext4TimeSpec>,
    pub mtime: Option<Ext4TimeSpec>,
    pub ctime: Option<Ext4TimeSpec>,
    pub crtime: Option<Ext4TimeSpec>,
    pub dtime: Ext4DtimeUpdate,
    pub clear_suid_sgid_on_write: bool,
    pub clear_suid_sgid_on_chown: bool,
}

impl Default for Ext4InodeMetadataUpdate {
    fn default() -> Self {
        Self {
            reason: Ext4MetadataReason::Read,
            mode: None,
            uid: None,
            gid: None,
            projid: None,
            atime: None,
            mtime: None,
            ctime: None,
            crtime: None,
            dtime: Ext4DtimeUpdate::Keep,
            clear_suid_sgid_on_write: false,
            clear_suid_sgid_on_chown: false,
        }
    }
}

impl Ext4InodeMetadataUpdate {
    pub(crate) fn create(mode: u16) -> Self {
        Self {
            reason: Ext4MetadataReason::Create,
            mode: Some(Ext4ModeUpdate::Replace(mode)),
            atime: Some(Ext4TimeSpec::Now),
            mtime: Some(Ext4TimeSpec::Now),
            ctime: Some(Ext4TimeSpec::Now),
            crtime: Some(Ext4TimeSpec::Now),
            dtime: Ext4DtimeUpdate::Clear,
            ..Default::default()
        }
    }

    pub(crate) fn read_access() -> Self {
        Self {
            reason: Ext4MetadataReason::Read,
            atime: Some(Ext4TimeSpec::Now),
            ..Default::default()
        }
    }

    pub(crate) fn write_access() -> Self {
        Self {
            reason: Ext4MetadataReason::Write,
            mtime: Some(Ext4TimeSpec::Now),
            ctime: Some(Ext4TimeSpec::Now),
            clear_suid_sgid_on_write: true,
            ..Default::default()
        }
    }

    pub(crate) fn truncate_access() -> Self {
        Self {
            reason: Ext4MetadataReason::Truncate,
            mtime: Some(Ext4TimeSpec::Now),
            ctime: Some(Ext4TimeSpec::Now),
            clear_suid_sgid_on_write: true,
            ..Default::default()
        }
    }

    pub(crate) fn chmod(mode: u16) -> Self {
        Self {
            reason: Ext4MetadataReason::Chmod,
            mode: Some(Ext4ModeUpdate::Chmod(mode)),
            ctime: Some(Ext4TimeSpec::Now),
            ..Default::default()
        }
    }

    pub(crate) fn chown(uid: Option<u32>, gid: Option<u32>) -> Self {
        Self {
            reason: Ext4MetadataReason::Chown,
            uid,
            gid,
            ctime: Some(Ext4TimeSpec::Now),
            clear_suid_sgid_on_chown: true,
            ..Default::default()
        }
    }

    pub(crate) fn utimens(atime: Ext4TimeSpec, mtime: Ext4TimeSpec) -> Self {
        let ctime = if matches!(atime, Ext4TimeSpec::Omit) && matches!(mtime, Ext4TimeSpec::Omit) {
            None
        } else {
            Some(Ext4TimeSpec::Now)
        };

        Self {
            reason: Ext4MetadataReason::Utimens,
            atime: Some(atime),
            mtime: Some(mtime),
            ctime,
            ..Default::default()
        }
    }

    pub(crate) fn link_count_change() -> Self {
        Self {
            reason: Ext4MetadataReason::LinkCount,
            ctime: Some(Ext4TimeSpec::Now),
            ..Default::default()
        }
    }

    pub(crate) fn parent_dir_change() -> Self {
        Self {
            reason: Ext4MetadataReason::ParentDir,
            mtime: Some(Ext4TimeSpec::Now),
            ctime: Some(Ext4TimeSpec::Now),
            ..Default::default()
        }
    }
}

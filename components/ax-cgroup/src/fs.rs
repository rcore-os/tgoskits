use core::time::Duration;

use crate::{CgroupError, CgroupResult};

/// Initialization data for a new directory and its initial interfaces.
/// The IDs are inode owners, not authorization credentials.
#[derive(Clone, Copy, Debug)]
pub struct CgroupFsInit {
    pub uid: u32,
    pub gid: u32,
    pub mode: u16,
    pub now: Duration,
}

impl CgroupFsInit {
    pub(crate) fn validate(self) -> CgroupResult<()> {
        if self.mode & !0o1777 != 0 {
            return Err(CgroupError::InvalidInput);
        }
        Ok(())
    }
}

/// A snapshot of one directory or control-file inode's attributes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CgroupFsAttributes {
    pub inode: u64,
    pub uid: u32,
    pub gid: u32,
    pub mode: u16,
    pub atime: Duration,
    pub mtime: Duration,
    pub ctime: Duration,
}

/// A field-wise attribute patch. Unspecified fields and inode identity are kept.
#[derive(Clone, Copy, Debug, Default)]
pub struct CgroupFsUpdate {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub mode: Option<u16>,
    pub atime: Option<Duration>,
    pub mtime: Option<Duration>,
    pub ctime: Option<Duration>,
}

impl CgroupFsAttributes {
    fn new(inode: u64, init: CgroupFsInit, mode: u16) -> Self {
        Self {
            inode,
            uid: init.uid,
            gid: init.gid,
            mode,
            atime: init.now,
            mtime: init.now,
            ctime: init.now,
        }
    }

    pub(crate) fn update(&mut self, update: CgroupFsUpdate) -> CgroupResult<()> {
        if let Some(mode) = update.mode {
            if mode & !0o7777 != 0 {
                return Err(CgroupError::InvalidInput);
            }
            self.mode = mode;
        }
        if let Some(uid) = update.uid {
            self.uid = uid;
        }
        if let Some(gid) = update.gid {
            self.gid = gid;
        }
        if let Some(atime) = update.atime {
            self.atime = atime;
        }
        if let Some(mtime) = update.mtime {
            self.mtime = mtime;
        }
        if let Some(ctime) = update.ctime {
            self.ctime = ctime;
        }
        Ok(())
    }
}

/// The control-file identities supported by the hierarchy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CgroupFileKind {
    Controllers,
    Procs,
    SubtreeControl,
    PidsMax,
    PidsCurrent,
    PidsPeak,
    PidsEvents,
}

impl CgroupFileKind {
    const INTERFACES: [(Self, &str, u16); 7] = [
        (Self::Controllers, "cgroup.controllers", 0o444),
        (Self::Procs, "cgroup.procs", 0o644),
        (Self::SubtreeControl, "cgroup.subtree_control", 0o644),
        (Self::PidsMax, "pids.max", 0o644),
        (Self::PidsCurrent, "pids.current", 0o444),
        (Self::PidsPeak, "pids.peak", 0o444),
        (Self::PidsEvents, "pids.events", 0o444),
    ];

    /// All recognized control-file kinds, including currently absent interfaces.
    pub const ALL: [Self; Self::INTERFACES.len()] = {
        let mut kinds = [Self::Controllers; Self::INTERFACES.len()];
        let mut i = 0;
        while i < kinds.len() {
            kinds[i] = Self::INTERFACES[i].0;
            i += 1;
        }
        kinds
    };

    /// Resolve a reserved interface name regardless of its current visibility.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The reserved control-file name.
    pub const fn name(self) -> &'static str {
        Self::INTERFACES[self as usize].1
    }

    /// Initial permissions determined by the interface's read/write behavior.
    pub const fn mode(self) -> u16 {
        Self::INTERFACES[self as usize].2
    }

    const fn requires_pids(self) -> bool {
        matches!(
            self,
            Self::PidsMax | Self::PidsCurrent | Self::PidsPeak | Self::PidsEvents
        )
    }
}

/// One independently owned inode within a cgroup node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CgroupFsTarget {
    Directory,
    File(CgroupFileKind),
}

impl CgroupFsTarget {
    const INODES_PER_NODE: u64 = CgroupFileKind::ALL.len() as u64 + 1;

    /// Leave room for every target, including interfaces installed later.
    pub(crate) const MAX_NODE_ID: u64 =
        (u64::MAX - (Self::INODES_PER_NODE - 1)) / Self::INODES_PER_NODE;

    pub(crate) const fn inode(self, node_id: u64) -> u64 {
        let slot = match self {
            Self::Directory => 0,
            Self::File(kind) => kind as u64 + 1,
        };
        // The allocator bounds node IDs before any target is constructed.
        let Some(base) = node_id.checked_mul(Self::INODES_PER_NODE) else {
            unreachable!()
        };
        let Some(inode) = base.checked_add(slot) else {
            unreachable!()
        };
        inode
    }
}

pub(crate) struct CgroupFsState {
    directory: CgroupFsAttributes,
    files: [Option<CgroupFsAttributes>; CgroupFileKind::ALL.len()],
}

impl CgroupFsState {
    pub(crate) fn new(node_id: u64, init: CgroupFsInit, pids: bool) -> Self {
        Self {
            directory: CgroupFsAttributes::new(
                CgroupFsTarget::Directory.inode(node_id),
                init,
                init.mode,
            ),
            files: CgroupFileKind::ALL.map(|kind| {
                (!kind.requires_pids() || pids).then(|| Self::file_attributes(node_id, kind, init))
            }),
        }
    }

    pub(crate) fn install_pids(&mut self, node_id: u64, init: CgroupFsInit) {
        for kind in CgroupFileKind::ALL
            .into_iter()
            .filter(|kind| kind.requires_pids())
        {
            self.files[kind as usize] = Some(Self::file_attributes(node_id, kind, init));
        }
    }

    fn file_attributes(
        node_id: u64,
        kind: CgroupFileKind,
        init: CgroupFsInit,
    ) -> CgroupFsAttributes {
        CgroupFsAttributes::new(CgroupFsTarget::File(kind).inode(node_id), init, kind.mode())
    }

    pub(crate) fn attributes(&self, target: CgroupFsTarget) -> CgroupResult<CgroupFsAttributes> {
        match target {
            CgroupFsTarget::Directory => Ok(self.directory),
            CgroupFsTarget::File(kind) => self.files[kind as usize].ok_or(CgroupError::NotFound),
        }
    }

    pub(crate) fn update(
        &mut self,
        target: CgroupFsTarget,
        update: CgroupFsUpdate,
    ) -> CgroupResult<()> {
        let attributes = match target {
            CgroupFsTarget::Directory => &mut self.directory,
            CgroupFsTarget::File(kind) => self.files[kind as usize]
                .as_mut()
                .ok_or(CgroupError::NotFound)?,
        };
        attributes.update(update)
    }

    pub(crate) fn touch_directory(&mut self, now: Duration) {
        self.directory.mtime = now;
        self.directory.ctime = now;
    }
}

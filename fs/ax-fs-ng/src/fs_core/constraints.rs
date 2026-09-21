use axfs_ng_vfs::Location;
use bitflags::bitflags;

bitflags! {
    /// Path-walk restrictions mirroring Linux openat2's `RESOLVE_*` flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ResolveFlags: u8 {
        /// Refuse to resolve above the starting directory (`RESOLVE_BENEATH`).
        const BENEATH = 1 << 0;
        /// Treat the starting directory as a chroot root (`RESOLVE_IN_ROOT`).
        const IN_ROOT = 1 << 1;
        /// Refuse to cross mount boundaries in either direction
        /// (`RESOLVE_NO_XDEV`).
        const NO_XDEV = 1 << 2;
        /// Refuse to follow any symbolic link (`RESOLVE_NO_SYMLINKS`).
        const NO_SYMLINKS = 1 << 3;
        /// Refuse to follow procfs-style magic links while still following
        /// ordinary symlinks (`RESOLVE_NO_MAGICLINKS`).
        const NO_MAGICLINKS = 1 << 4;
    }
}

/// Constraints applied while walking a user-supplied path.
///
/// The walk always starts at the caller-chosen directory (the dirfd), so
/// `RESOLVE_BENEATH` needs no explicit base: staying beneath the start is
/// enforced by tracking the walk depth and rejecting absolute components and
/// `..` at depth zero. `RESOLVE_IN_ROOT` does need the root location so
/// absolute components and clamped `..` restart there.
///
/// See `man 2 openat2` for the Linux semantics each flag mirrors.
#[derive(Debug, Clone)]
pub struct ResolveConstraints {
    flags: ResolveFlags,
    /// The `RESOLVE_IN_ROOT` root; absolute components and `..`-at-root clamp
    /// here instead of the filesystem root.
    root: Option<Location>,
}

impl ResolveConstraints {
    /// Creates an empty constraint set: path walking behaves like plain
    /// `openat(2)`.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            flags: ResolveFlags::empty(),
            root: None,
        }
    }

    /// Treats the given location as the resolution root
    /// (`RESOLVE_IN_ROOT`).
    pub fn in_root(mut self, root: Location) -> Self {
        self.flags |= ResolveFlags::IN_ROOT;
        self.root = Some(root);
        self
    }

    /// Marks `RESOLVE_IN_ROOT` before the root location is known; call
    /// [`Self::in_root`] afterwards to attach it. Translation layers that
    /// decode flags before resolving their dirfd use this two-phase form.
    pub fn mark_in_root(mut self) -> Self {
        self.flags |= ResolveFlags::IN_ROOT;
        self
    }

    /// Refuses to resolve above the starting directory (`RESOLVE_BENEATH`).
    pub fn beneath(mut self) -> Self {
        self.flags |= ResolveFlags::BENEATH;
        self
    }

    /// Refuses to cross mount boundaries (`RESOLVE_NO_XDEV`).
    pub fn no_xdev(mut self) -> Self {
        self.flags |= ResolveFlags::NO_XDEV;
        self
    }

    /// Refuses to follow symbolic links (`RESOLVE_NO_SYMLINKS`).
    pub fn no_symlinks(mut self) -> Self {
        self.flags |= ResolveFlags::NO_SYMLINKS;
        self
    }

    /// Refuses to follow magic links (`RESOLVE_NO_MAGICLINKS`).
    pub fn no_magiclinks(mut self) -> Self {
        self.flags |= ResolveFlags::NO_MAGICLINKS;
        self
    }

    /// Returns whether no restriction is active, in which case callers may
    /// take the unconstrained fast path.
    pub fn is_unconstrained(&self) -> bool {
        self.flags.is_empty()
    }

    /// Whether the walk must stay beneath its starting directory.
    pub fn is_beneath(&self) -> bool {
        self.flags.contains(ResolveFlags::BENEATH)
    }

    /// Whether the starting directory acts as the resolution root.
    pub fn is_in_root(&self) -> bool {
        self.flags.contains(ResolveFlags::IN_ROOT)
    }

    /// Whether mount boundary crossings are rejected.
    pub fn is_no_xdev(&self) -> bool {
        self.flags.contains(ResolveFlags::NO_XDEV)
    }

    /// Whether symbolic links are rejected outright.
    pub fn is_no_symlinks(&self) -> bool {
        self.flags.contains(ResolveFlags::NO_SYMLINKS)
    }

    /// Whether magic links are rejected.
    pub fn is_no_magiclinks(&self) -> bool {
        self.flags.contains(ResolveFlags::NO_MAGICLINKS)
    }

    /// The `RESOLVE_IN_ROOT` root location, if set.
    pub fn root(&self) -> Option<&Location> {
        self.root.as_ref()
    }
}

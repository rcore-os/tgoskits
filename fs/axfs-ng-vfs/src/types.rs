use core::{fmt::Debug, time::Duration};

/// Filesystem node type.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NodeType {
    Unknown         = 0,
    Fifo            = 0o1,
    CharacterDevice = 0o2,
    Directory       = 0o4,
    BlockDevice     = 0o6,
    RegularFile     = 0o10,
    Symlink         = 0o12,
    Socket          = 0o14,
}

impl From<u8> for NodeType {
    fn from(value: u8) -> Self {
        match value {
            0o1 => Self::Fifo,
            0o2 => Self::CharacterDevice,
            0o4 => Self::Directory,
            0o6 => Self::BlockDevice,
            0o10 => Self::RegularFile,
            0o12 => Self::Symlink,
            0o14 => Self::Socket,
            _ => Self::Unknown,
        }
    }
}

bitflags::bitflags! {
    /// Inode permission mode.
    #[derive(Debug, Clone, Copy)]
    pub struct NodePermission: u16 {
        /// Set user ID on execution.
        const SET_UID = 0o4000;
        /// Set group ID on execution.
        const SET_GID = 0o2000;
        /// Sticky bit.
        const STICKY = 0o1000;

        /// Owner has read permission.
        const OWNER_READ = 0o400;
        /// Owner has write permission.
        const OWNER_WRITE = 0o200;
        /// Owner has execute permission.
        const OWNER_EXEC = 0o100;

        /// Group has read permission.
        const GROUP_READ = 0o40;
        /// Group has write permission.
        const GROUP_WRITE = 0o20;
        /// Group has execute permission.
        const GROUP_EXEC = 0o10;

        /// Others have read permission.
        const OTHER_READ = 0o4;
        /// Others have write permission.
        const OTHER_WRITE = 0o2;
        /// Others have execute permission.
        const OTHER_EXEC = 0o1;
    }
}

impl Default for NodePermission {
    fn default() -> Self {
        Self::from_bits_truncate(0o666)
    }
}

/// Filesystem node metadata.
#[derive(Clone, Debug)]
pub struct Metadata {
    /// ID of device containing file
    pub device: u64,
    /// Inode number
    pub inode: u64,
    /// Number of hard links
    pub nlink: u64,
    /// Permission mode
    pub mode: NodePermission,
    /// Node type
    pub node_type: NodeType,
    /// User ID of owner
    pub uid: u32,
    /// Group ID of owner
    pub gid: u32,
    /// Total size in bytes
    pub size: u64,
    /// Block size for filesystem I/O
    pub block_size: u64,
    /// Number of 512B blocks allocated
    pub blocks: u64,
    /// Device ID (if special file)
    pub rdev: DeviceId,

    /// Time of last access
    pub atime: Duration,
    /// Time of last modification
    pub mtime: Duration,
    /// Time of last status change
    pub ctime: Duration,
}

/// Credentials used by filesystem mutation authorization.
///
/// The VFS does not own a task or a credential namespace, so callers must
/// provide a snapshot for each mutation.  The snapshot deliberately contains
/// only the identity and capabilities needed by DAC and sticky-directory
/// checks; filesystem implementations must not infer these values from the
/// host or a global default.
#[derive(Clone, Copy, Debug)]
pub struct MutationCredentials<'a> {
    /// Filesystem user ID used for DAC and ownership checks.
    pub fsuid: u32,
    /// Filesystem group ID used for DAC checks.
    pub fsgid: u32,
    /// Supplementary groups used for DAC group selection.
    pub supplementary_gids: &'a [u32],
    /// Whether the caller has `CAP_DAC_OVERRIDE`.
    pub cap_dac_override: bool,
    /// Whether the caller has `CAP_DAC_READ_SEARCH`.
    pub cap_dac_read_search: bool,
    /// Whether the caller has `CAP_FOWNER`.
    pub cap_fowner: bool,
}

impl MutationCredentials<'static> {
    /// Credentials for trusted filesystem initialization paths.
    pub const fn root() -> Self {
        Self {
            fsuid: 0,
            fsgid: 0,
            supplementary_gids: &[],
            cap_dac_override: true,
            cap_dac_read_search: true,
            cap_fowner: true,
        }
    }
}

impl MutationCredentials<'_> {
    /// Returns whether the credential is a member of `gid`.
    pub fn in_group(&self, gid: u32) -> bool {
        self.fsgid == gid || self.supplementary_gids.contains(&gid)
    }
}

/// Filesystem node metadata update.
#[derive(Default, Clone, Debug)]
pub struct MetadataUpdate {
    /// Permission mode
    pub mode: Option<NodePermission>,
    /// The owner (uid, gid)
    pub owner: Option<(u32, u32)>,
    /// Device ID (for char/block device nodes created via mknod)
    pub rdev: Option<DeviceId>,

    /// Time of last access
    pub atime: Option<Duration>,
    /// Time of last modification
    pub mtime: Option<Duration>,
}

/// Device Id
#[derive(Default, Clone, PartialEq, Eq, Copy)]
pub struct DeviceId(pub u64);

impl DeviceId {
    pub const fn new(major: u32, minor: u32) -> Self {
        let major = major as u64;
        let minor = minor as u64;
        Self(
            (major & 0xffff_f000) << 32
                | (major & 0x0000_0fff) << 8
                | (minor & 0xffff_ff00) << 12
                | (minor & 0x0000_00ff),
        )
    }

    pub const fn major(&self) -> u32 {
        ((self.0 >> 32) & 0xffff_f000 | (self.0 >> 8) & 0x0000_0fff) as u32
    }

    pub const fn minor(&self) -> u32 {
        ((self.0 >> 12) & 0xffff_ff00 | self.0 & 0x0000_00ff) as u32
    }
}

impl Debug for DeviceId {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("DeviceId")
            .field("major", &self.major())
            .field("minor", &self.minor())
            .finish()
    }
}

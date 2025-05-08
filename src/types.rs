use core::time::Duration;

/// Filesystem node type.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NodeType {
    Fifo = 0o1,
    CharacterDevice = 0o2,
    Directory = 0o4,
    BlockDevice = 0o6,
    RegularFile = 0o10,
    Symlink = 0o12,
    Socket = 0o14,
}
impl TryFrom<u8> for NodeType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0o1 => Ok(Self::Fifo),
            0o2 => Ok(Self::CharacterDevice),
            0o4 => Ok(Self::Directory),
            0o6 => Ok(Self::BlockDevice),
            0o10 => Ok(Self::RegularFile),
            0o12 => Ok(Self::Symlink),
            0o14 => Ok(Self::Socket),
            _ => Err(()),
        }
    }
}

bitflags::bitflags! {
    /// Inode permission mode.
    #[derive(Debug, Clone, Copy)]
    pub struct NodePermission: u16 {
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

    /// Time of last access
    pub atime: Duration,
    /// Time of last modification
    pub mtime: Duration,
    /// Time of last status change
    pub ctime: Duration,
}

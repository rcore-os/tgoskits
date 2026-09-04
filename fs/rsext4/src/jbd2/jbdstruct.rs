//! Core JBD2 on-disk and in-memory data structures.

use alloc::{boxed::Box, vec::Vec};
use core::convert::TryInto;

use crate::{
    bmalloc::AbsoluteBN,
    config::*,
    endian::*,
    error::{Ext4Error, Ext4Result},
};
pub const JOURNAL_FILE_INODE: u64 = 8;
/// ext4 reserves inode 8 for the journal file.
pub const JBD2_MAGIC: u32 = 0xC03B_3998u32; // jbd2 magic number (on-disk big-endian)
pub const DEFAULT_JOURNAL_SIZE_BYTES: u32 = 32 * 1024 * 1024;
pub const CREATED_JOURNAL_BLOCK_COUNT: u32 = 4096;
pub const JOURNAL_BLOCK_COUNT: u32 = DEFAULT_JOURNAL_SIZE_BYTES / BLOCK_SIZE_U32;
pub const JOURNAL_ESCAPE: u16 = 0x1;
pub const JBD2_FLAG_SAME_UUID: u16 = 0x2;
pub const JBD2_FLAG_LAST_TAG: u16 = 0x8;
pub const JBD2_BLOCKTYPE_DESCRIPTOR: u32 = 1;
pub const JBD2_BLOCKTYPE_COMMIT: u32 = 2;
pub const JBD2_BLOCKTYPE_SUPERBLOCK_V1: u32 = 3;
pub const JBD2_BLOCKTYPE_SUPERBLOCK_V2: u32 = 4;
pub const JBD2_BLOCKTYPE_REVOKE: u32 = 5;
pub const JBD2_DESCRIPTOR_HEADER_SIZE: usize = 12;
pub const JBD2_TAG_SIZE: usize = 8;
pub const JBD2_TAG_BLOCKNR_HIGH_SIZE: usize = 4;
pub const JBD2_TAG3_SIZE: usize = 16;
pub const JBD2_UUID_SIZE: usize = 16;
pub const JBD2_CRC32_CHKSUM: u8 = 1;
pub const JBD2_CRC32_CHKSUM_SIZE: u8 = 4;
pub const JBD2_CRC32C_CHKSUM: u8 = 4; // JBD2 checksum type for CRC32C
pub const JBD2_FEATURE_COMPAT_CHECKSUM: u32 = 0x0000_0001;
pub const JBD2_FEATURE_INCOMPAT_REVOKE: u32 = 0x0000_0001;
pub const JBD2_FEATURE_INCOMPAT_64BIT: u32 = 0x0000_0002;
pub const JBD2_FEATURE_INCOMPAT_CSUM_V2: u32 = 0x0000_0008;
pub const JBD2_FEATURE_INCOMPAT_CSUM_V3: u32 = 0x0000_0010;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Jbd2ChecksumMode {
    None,
    CompatChecksum,
    CsumV2,
    CsumV3,
}

impl Jbd2ChecksumMode {
    pub(crate) const fn has_block_checksums(self) -> bool {
        matches!(self, Self::CsumV2 | Self::CsumV3)
    }

    const fn descriptor_tail_bytes(self) -> usize {
        if self.has_block_checksums() {
            core::mem::size_of::<u32>()
        } else {
            0
        }
    }

    pub(crate) const fn tag_bytes(self, has_64bit: bool) -> usize {
        match self {
            Self::CsumV3 => JBD2_TAG3_SIZE,
            Self::CsumV2 => {
                JBD2_TAG_SIZE
                    + core::mem::size_of::<u16>()
                    + if has_64bit {
                        JBD2_TAG_BLOCKNR_HIGH_SIZE
                    } else {
                        0
                    }
            }
            Self::None | Self::CompatChecksum => {
                JBD2_TAG_SIZE
                    + if has_64bit {
                        JBD2_TAG_BLOCKNR_HIGH_SIZE
                    } else {
                        0
                    }
            }
        }
    }
}
#[repr(C)]
/// One journaled metadata update: `(target physical block, serialized block)`.
pub struct Jbd2Update(pub AbsoluteBN, pub Box<[u8]>);

/// Admission phase of the transaction that owns new journal handles.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Jbd2RunningTransactionPhase {
    #[default]
    Running,
    Locked,
    Switch,
}

/// Metadata owned by the transaction that accepts new journal handles.
#[derive(Default)]
pub(crate) struct Jbd2RunningTransaction {
    pub(crate) phase: Jbd2RunningTransactionPhase,
    pub(crate) updates: Vec<Jbd2Update>,
    pub(crate) revoked_blocks: Vec<AbsoluteBN>,
}

/// Commit phases that mirror Linux JBD2's single commit-thread transitions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Jbd2CommitPhase {
    Flush,
    Commit,
    DataFlush,
    JournalFlush,
}

/// One closed transaction while its journal commit record is being published.
pub(crate) struct Jbd2CommittingTransaction {
    pub(crate) sequence: u32,
    pub(crate) log_start: u32,
    pub(crate) phase: Jbd2CommitPhase,
    pub(crate) updates: Vec<Jbd2Update>,
    pub(crate) revoked_blocks: Vec<AbsoluteBN>,
}

/// One transaction whose commit record is durable but whose home writes may
/// still be pending.
pub(crate) struct Jbd2CheckpointTransaction {
    pub(crate) sequence: u32,
    pub(crate) log_start: u32,
    pub(crate) log_records: usize,
    pub(crate) updates: Vec<Jbd2Update>,
    pub(crate) revoked_blocks: Vec<AbsoluteBN>,
}

#[repr(C)]
pub struct JBD2DEVSYSTEM {
    pub jbd2_super_block: JournalSuperBlock,
    pub start_block: AbsoluteBN, // Physical block containing the journal superblock.
    pub max_len: u32,            // Total number of blocks in the journal area.
    pub head: u32,               // Next writable relative log block.
    pub sequence: u32,           // Next expected transaction sequence ID.
    pub(crate) running_transaction: Jbd2RunningTransaction,
    pub(crate) committing_transaction: Option<Jbd2CommittingTransaction>,
    pub(crate) checkpoint_transactions: Vec<Jbd2CheckpointTransaction>,
    pub(crate) used_log_records: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JournalHeaderS {
    pub h_magic: u32,     // __be32: magic number (0xC03B3998)
    pub h_blocktype: u32, // __be32: block type (descriptor, commit, superblock, ...)
    pub h_sequence: u32,  // __be32: transaction sequence id
}
impl Default for JournalHeaderS {
    /// Defaults to a superblock header record.
    fn default() -> Self {
        JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: JBD2_BLOCKTYPE_SUPERBLOCK_V2,
            h_sequence: 0,
        }
    }
}

impl DiskFormat for JournalHeaderS {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let h_magic = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        let h_blocktype = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
        let h_sequence = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
        JournalHeaderS {
            h_magic,
            h_blocktype,
            h_sequence,
        }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        bytes[0..4].copy_from_slice(&self.h_magic.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.h_blocktype.to_be_bytes());
        bytes[8..12].copy_from_slice(&self.h_sequence.to_be_bytes());
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JournalSuperBlock {
    // Offset 0x0 - 0xB: journal_header_t (12 bytes)
    pub s_header: JournalHeaderS,

    // Static information describing the journal
    pub s_blocksize: u32, // 0xC  __be32
    pub s_maxlen: u32,    // 0x10 __be32: total number of blocks in journal
    pub s_first: u32,     // 0x14 __be32: first block of log information

    // Dynamic information describing the current state of the log
    pub s_sequence: u32, // 0x18 __be32: first commit id expected in log
    pub s_start: u32,    // 0x1C __be32: block number of start of log
    pub s_errno: u32,    // 0x20 __be32: error value

    // The remaining fields are valid in a v2 superblock
    pub s_feature_compat: u32,    // 0x24 __be32
    pub s_feature_incompat: u32,  // 0x28 __be32
    pub s_feature_ro_compat: u32, // 0x2C __be32
    pub s_uuid: [u8; 16],         // 0x30 __u8[16]
    pub s_nr_users: u32,          // 0x40 __be32
    pub s_dynsuper: u32,          // 0x44 __be32
    pub s_max_transaction: u32,   // 0x48 __be32
    pub s_max_trans_data: u32,    // 0x4C __be32
    pub s_checksum_type: u8,      // 0x50 __u8
    pub s_padding2: [u8; 3],      // 0x51 padding

    // padding up to 0xFC
    pub s_padding: [u32; 42], // 0x54..0xFC
    pub s_checksum: u32,      // 0xFC __be32: checksum of superblock (with this zeroed)

    // 0x100 .. 0x3FF: list of users (16 * 48 = 768 bytes)
    pub s_users: [u8; 16 * 48], // ids of filesystems sharing the log
}

impl Default for JournalSuperBlock {
    /// Creates a journal superblock template.
    ///
    /// Callers are expected to override `s_maxlen` with the real journal size.
    fn default() -> Self {
        let header = JournalHeaderS::default();
        JournalSuperBlock {
            s_header: header,
            s_blocksize: BLOCK_SIZE_U32,
            s_maxlen: 4096,
            s_first: 1,
            s_sequence: 1,
            s_start: 0,
            s_errno: 0,
            s_feature_compat: 0,
            s_feature_incompat: 0,
            s_feature_ro_compat: 0,
            s_uuid: [0; 16],
            s_nr_users: 1,
            s_dynsuper: 0,
            s_max_transaction: JOURNAL_BLOCK_COUNT,
            s_max_trans_data: JOURNAL_BLOCK_COUNT * 10,
            s_checksum_type: 0,
            s_padding2: [0; 3],
            s_padding: [0; 42],
            s_checksum: 0,
            s_users: [0; 768],
        }
    }
}

impl JournalSuperBlock {
    /// Fixed size of `journal_superblock_s` through Linux 7.1.
    pub const DISK_SIZE: usize = 1024;

    /// Returns whether only the version-1 prefix fields are meaningful.
    pub const fn is_v1(&self) -> bool {
        self.s_header.h_blocktype == JBD2_BLOCKTYPE_SUPERBLOCK_V1
    }

    pub(crate) fn checksum_mode(&self) -> Ext4Result<Jbd2ChecksumMode> {
        if self.is_v1() {
            return Ok(Jbd2ChecksumMode::None);
        }

        let compat_checksum = self.s_feature_compat & JBD2_FEATURE_COMPAT_CHECKSUM != 0;
        let csum_v2 = self.s_feature_incompat & JBD2_FEATURE_INCOMPAT_CSUM_V2 != 0;
        let csum_v3 = self.s_feature_incompat & JBD2_FEATURE_INCOMPAT_CSUM_V3 != 0;
        if usize::from(compat_checksum) + usize::from(csum_v2) + usize::from(csum_v3) > 1 {
            return Err(Ext4Error::unsupported().with_operation("jbd2:checksum_features"));
        }

        let mode = if compat_checksum {
            Jbd2ChecksumMode::CompatChecksum
        } else if csum_v2 {
            Jbd2ChecksumMode::CsumV2
        } else if csum_v3 {
            Jbd2ChecksumMode::CsumV3
        } else {
            Jbd2ChecksumMode::None
        };
        let expected_type = if mode.has_block_checksums() {
            JBD2_CRC32C_CHKSUM
        } else {
            0
        };
        if self.s_checksum_type != expected_type {
            return Err(Ext4Error::unsupported().with_operation("jbd2:checksum_features"));
        }
        Ok(mode)
    }

    /// Returns Linux's continuation capacity for one descriptor block.
    ///
    /// In addition to the UUID following the first tag, JBD2 closes the
    /// descriptor unless another tag, UUID, and checksum tail could fit. This
    /// deliberately leaves one UUID-sized reserve instead of packing the last
    /// otherwise byte-aligned tag into a non-final descriptor.
    pub(crate) fn descriptor_tag_capacity(&self, block_size: usize) -> Ext4Result<usize> {
        let feature_incompat = if self.is_v1() {
            0
        } else {
            self.s_feature_incompat
        };
        let has_64bit = feature_incompat & JBD2_FEATURE_INCOMPAT_64BIT != 0;
        let checksum_mode = self.checksum_mode()?;
        let descriptor_tail = checksum_mode.descriptor_tail_bytes();
        let descriptor_end = block_size
            .checked_sub(descriptor_tail)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:descriptor_capacity"))?;
        let fixed_descriptor_bytes = JBD2_DESCRIPTOR_HEADER_SIZE
            .checked_add(JBD2_UUID_SIZE)
            .and_then(|bytes| bytes.checked_add(JBD2_UUID_SIZE))
            .ok_or_else(Ext4Error::overflow)?;
        let tag_bytes = checksum_mode.tag_bytes(has_64bit);
        let capacity = descriptor_end
            .checked_sub(fixed_descriptor_bytes)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:descriptor_capacity"))?
            / tag_bytes;
        if capacity == 0 {
            return Err(Ext4Error::no_space().with_operation("jbd2:descriptor_capacity"));
        }
        Ok(capacity)
    }

    /// Returns the number of revoke records carried by one revoke block.
    pub(crate) fn revoke_records_per_block(&self, block_size: usize) -> Ext4Result<usize> {
        let feature_incompat = if self.is_v1() {
            0
        } else {
            self.s_feature_incompat
        };
        let entry_size = if feature_incompat & JBD2_FEATURE_INCOMPAT_64BIT != 0 {
            core::mem::size_of::<u64>()
        } else {
            core::mem::size_of::<u32>()
        };
        let checksum_tail = if self.checksum_mode()?.has_block_checksums() {
            core::mem::size_of::<u32>()
        } else {
            0
        };
        let record_bytes = block_size
            .checked_sub(core::mem::size_of::<Jbd2JournalRevokeHeadS>())
            .and_then(|bytes| bytes.checked_sub(checksum_tail))
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:revoke_capacity"))?;
        let capacity = record_bytes / entry_size;
        if capacity == 0 {
            return Err(Ext4Error::no_space().with_operation("jbd2:revoke_capacity"));
        }
        Ok(capacity)
    }

    /// Decodes the fixed JBD2 superblock prefix from a journal block.
    pub fn decode_checked(bytes: &[u8]) -> Ext4Result<Self> {
        if bytes.len() < Self::DISK_SIZE {
            return Err(Ext4Error::corrupted().with_operation("jbd2:superblock_decode_size"));
        }
        Ok(Self::from_validated_disk_bytes(bytes))
    }

    /// Encodes the fixed JBD2 superblock prefix into a journal block.
    pub(crate) fn encode_checked(&self, bytes: &mut [u8]) -> Ext4Result<()> {
        if bytes.len() < Self::DISK_SIZE {
            return Err(Ext4Error::corrupted().with_operation("jbd2:superblock_encode_size"));
        }
        self.write_validated_disk_bytes(bytes);
        Ok(())
    }

    fn from_validated_disk_bytes(bytes: &[u8]) -> Self {
        let s_header = JournalHeaderS::from_disk_bytes(&bytes[0..12]);

        let s_blocksize = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
        let s_maxlen = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let s_first = u32::from_be_bytes(bytes[20..24].try_into().unwrap());

        let s_sequence = u32::from_be_bytes(bytes[24..28].try_into().unwrap());
        let s_start = u32::from_be_bytes(bytes[28..32].try_into().unwrap());
        let s_errno = u32::from_be_bytes(bytes[32..36].try_into().unwrap());

        let s_feature_compat = u32::from_be_bytes(bytes[36..40].try_into().unwrap());
        let s_feature_incompat = u32::from_be_bytes(bytes[40..44].try_into().unwrap());
        let s_feature_ro_compat = u32::from_be_bytes(bytes[44..48].try_into().unwrap());

        let mut s_uuid = [0u8; 16];
        s_uuid.copy_from_slice(&bytes[48..64]);

        let s_nr_users = u32::from_be_bytes(bytes[64..68].try_into().unwrap());
        let s_dynsuper = u32::from_be_bytes(bytes[68..72].try_into().unwrap());
        let s_max_transaction = u32::from_be_bytes(bytes[72..76].try_into().unwrap());
        let s_max_trans_data = u32::from_be_bytes(bytes[76..80].try_into().unwrap());

        let s_checksum_type = bytes[80];
        let mut s_padding2 = [0u8; 3];
        s_padding2.copy_from_slice(&bytes[81..84]);

        let mut s_padding = [0u32; 42];
        let mut off = 84usize;
        for elem in &mut s_padding {
            *elem = u32::from_be_bytes(bytes[off..off + 4].try_into().unwrap());
            off += 4;
        }

        let s_checksum = u32::from_be_bytes(bytes[0xFC..0x100].try_into().unwrap());

        let mut s_users = [0u8; 16 * 48];
        s_users.copy_from_slice(&bytes[0x100..0x100 + 16 * 48]);

        JournalSuperBlock {
            s_header,
            s_blocksize,
            s_maxlen,
            s_first,
            s_sequence,
            s_start,
            s_errno,
            s_feature_compat,
            s_feature_incompat,
            s_feature_ro_compat,
            s_uuid,
            s_nr_users,
            s_dynsuper,
            s_max_transaction,
            s_max_trans_data,
            s_checksum_type,
            s_padding2,
            s_padding,
            s_checksum,
            s_users,
        }
    }

    fn write_validated_disk_bytes(&self, bytes: &mut [u8]) {
        self.s_header.to_disk_bytes(&mut bytes[0..12]);
        bytes[12..16].copy_from_slice(&self.s_blocksize.to_be_bytes());
        bytes[16..20].copy_from_slice(&self.s_maxlen.to_be_bytes());
        bytes[20..24].copy_from_slice(&self.s_first.to_be_bytes());

        bytes[24..28].copy_from_slice(&self.s_sequence.to_be_bytes());
        bytes[28..32].copy_from_slice(&self.s_start.to_be_bytes());
        bytes[32..36].copy_from_slice(&self.s_errno.to_be_bytes());

        bytes[36..40].copy_from_slice(&self.s_feature_compat.to_be_bytes());
        bytes[40..44].copy_from_slice(&self.s_feature_incompat.to_be_bytes());
        bytes[44..48].copy_from_slice(&self.s_feature_ro_compat.to_be_bytes());

        bytes[48..64].copy_from_slice(&self.s_uuid);

        bytes[64..68].copy_from_slice(&self.s_nr_users.to_be_bytes());
        bytes[68..72].copy_from_slice(&self.s_dynsuper.to_be_bytes());
        bytes[72..76].copy_from_slice(&self.s_max_transaction.to_be_bytes());
        bytes[76..80].copy_from_slice(&self.s_max_trans_data.to_be_bytes());

        bytes[80] = self.s_checksum_type;
        bytes[81..84].copy_from_slice(&self.s_padding2);

        let mut off = 84usize;
        for padding in &self.s_padding {
            bytes[off..off + 4].copy_from_slice(&padding.to_be_bytes());
            off += 4;
        }

        bytes[0xFC..0x100].copy_from_slice(&self.s_checksum.to_be_bytes());
        bytes[0x100..0x100 + 16 * 48].copy_from_slice(&self.s_users);
    }
}

impl DiskFormat for JournalSuperBlock {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        Self::from_validated_disk_bytes(bytes)
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        self.write_validated_disk_bytes(bytes);
    }

    fn disk_size() -> usize {
        Self::DISK_SIZE
    }
}

// Descriptor / Tag structures

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JournalBlockTagS {
    // Basic (v1/v2) tag layout
    pub t_blocknr: u32,  // __be32: lower 32-bits of target block number
    pub t_checksum: u16, // __be16: checksum (lower 16 bits)
    pub t_flags: u16,    /* __be16: flags (escaped, same UUID, last tag, ...)
                          * Optionally followed by __be32 t_blocknr_high (when 64-bit support)
                          * and optionally a 16-byte uuid, depending on flags/features. */
}

impl DiskFormat for JournalBlockTagS {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let t_blocknr = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        let t_checksum = u16::from_be_bytes(bytes[4..6].try_into().unwrap());
        let t_flags = u16::from_be_bytes(bytes[6..8].try_into().unwrap());
        JournalBlockTagS {
            t_blocknr,
            t_checksum,
            t_flags,
        }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        bytes[0..4].copy_from_slice(&self.t_blocknr.to_be_bytes());
        bytes[4..6].copy_from_slice(&self.t_checksum.to_be_bytes());
        bytes[6..8].copy_from_slice(&self.t_flags.to_be_bytes());
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JournalBlockTag3S {
    // v3 tag layout used when JBD2_FEATURE_INCOMPAT_CSUM_V3 is set
    pub t_blocknr: u32,      // __be32: lower 32 bits
    pub t_flags: u32,        // __be32: flags (includes LAST flag, SAME_UUID, ESCAPED)
    pub t_blocknr_high: u32, // __be32: upper 32 bits when 64-bit support present
    pub t_checksum: u32,     /* __be32: full checksum
                              * Optionally followed by a uuid (16 bytes) unless SAME_UUID flag set. */
}

impl DiskFormat for JournalBlockTag3S {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let t_blocknr = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        let t_flags = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
        let t_blocknr_high = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
        let t_checksum = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
        JournalBlockTag3S {
            t_blocknr,
            t_flags,
            t_blocknr_high,
            t_checksum,
        }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        bytes[0..4].copy_from_slice(&self.t_blocknr.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.t_flags.to_be_bytes());
        bytes[8..12].copy_from_slice(&self.t_blocknr_high.to_be_bytes());
        bytes[12..16].copy_from_slice(&self.t_checksum.to_be_bytes());
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Jbd2JournalBlockTail {
    pub t_checksum: u32, // __be32: checksum for descriptor block (with this zeroed)
}

impl DiskFormat for Jbd2JournalBlockTail {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let t_checksum = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        Jbd2JournalBlockTail { t_checksum }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        bytes[0..4].copy_from_slice(&self.t_checksum.to_be_bytes());
    }
}

// Revocation block header
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Jbd2JournalRevokeHeadS {
    pub r_header: JournalHeaderS, // common header
    pub r_count: u32,             /* __be32: number of bytes used in this block
                                   * Followed by an array of block numbers (4 or 8 bytes each depending on 64-bit support) */
}

impl DiskFormat for Jbd2JournalRevokeHeadS {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let r_header = JournalHeaderS::from_disk_bytes(&bytes[0..12]);
        let r_count = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
        Jbd2JournalRevokeHeadS { r_header, r_count }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        self.r_header.to_disk_bytes(&mut bytes[0..12]);
        bytes[12..16].copy_from_slice(&self.r_count.to_be_bytes());
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Jbd2JournalRevokeTail {
    pub r_checksum: u32, // __be32: checksum of uuid + revoke block
}

impl DiskFormat for Jbd2JournalRevokeTail {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let r_checksum = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        Jbd2JournalRevokeTail { r_checksum }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        bytes[0..4].copy_from_slice(&self.r_checksum.to_be_bytes());
    }
}

/// Size of `struct commit_header` in the JBD2 wire format.
///
/// This must not use `size_of::<CommitHeader>()` because the Rust structure can
/// contain trailing alignment padding that is absent from the on-disk record.
pub(crate) const JBD2_COMMIT_HEADER_SIZE: usize = 60;

// Commit block header
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CommitHeader {
    pub h_header: JournalHeaderS, // common header (12 bytes)
    pub h_chksum_type: u8,        // 0xC  checksum type: 1=crc32,2=md5,3=sha1,4=crc32c
    pub h_chksum_size: u8,        // 0xD  size in bytes of checksum
    pub h_padding: [u8; 2],       // 0xE  padding
    pub h_chksum: [u32; 8],       // 0x10..0x2F: space for checksums (32 bytes)
    pub h_commit_sec: u64,        // 0x30 __be64: commit time seconds since epoch
    pub h_commit_nsec: u32,       // 0x38 __be32: commit time nanoseconds
}

impl DiskFormat for CommitHeader {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let h_header = JournalHeaderS::from_disk_bytes(&bytes[0..12]);
        let h_chksum_type = bytes[12];
        let h_chksum_size = bytes[13];
        let mut h_padding = [0u8; 2];
        h_padding.copy_from_slice(&bytes[14..16]);

        let mut h_chksum = [0u32; 8];
        let mut off = 16usize;
        for elem in &mut h_chksum {
            *elem = u32::from_be_bytes(bytes[off..off + 4].try_into().unwrap());
            off += 4;
        }

        let h_commit_sec = u64::from_be_bytes(bytes[48..56].try_into().unwrap());
        let h_commit_nsec = u32::from_be_bytes(bytes[56..60].try_into().unwrap());

        CommitHeader {
            h_header,
            h_chksum_type,
            h_chksum_size,
            h_padding,
            h_chksum,
            h_commit_sec,
            h_commit_nsec,
        }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        self.h_header.to_disk_bytes(&mut bytes[0..12]);
        bytes[12] = self.h_chksum_type;
        bytes[13] = self.h_chksum_size;
        bytes[14..16].copy_from_slice(&self.h_padding);

        let mut off = 16usize;
        for checksum in &self.h_chksum {
            bytes[off..off + 4].copy_from_slice(&checksum.to_be_bytes());
            off += 4;
        }

        bytes[48..56].copy_from_slice(&self.h_commit_sec.to_be_bytes());
        bytes[56..60].copy_from_slice(&self.h_commit_nsec.to_be_bytes());
    }
}

#[cfg(test)]
mod tests {
    use DiskFormat;

    use super::*;

    #[test]
    fn test_journal_header_roundtrip() {
        let hdr = JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: 2,
            h_sequence: 0x1122_3344,
        };
        let mut buf = [0u8; 12];
        hdr.to_disk_bytes(&mut buf);

        // Ensure big-endian ordering on disk
        assert_eq!(&buf[0..4], &JBD2_MAGIC.to_be_bytes());
        assert_eq!(&buf[4..8], &2u32.to_be_bytes());
        assert_eq!(&buf[8..12], &0x1122_3344u32.to_be_bytes());

        let parsed = JournalHeaderS::from_disk_bytes(&buf);
        assert_eq!(parsed.h_magic, JBD2_MAGIC);
        assert_eq!(parsed.h_blocktype, 2);
        assert_eq!(parsed.h_sequence, 0x1122_3344);
    }

    #[test]
    fn test_journal_superblock_roundtrip() {
        // build a sample superblock with distinct values
        let header = JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: JBD2_BLOCKTYPE_SUPERBLOCK_V1,
            h_sequence: 0xAABB_CCDD,
        };
        let sb = JournalSuperBlock {
            s_header: header,
            s_blocksize: 4096,
            s_maxlen: 1024,
            s_first: 2,
            s_sequence: 0x0102_0304,
            s_start: 0x1122_3344,
            s_errno: 0,
            s_feature_compat: 0x1,
            s_feature_incompat: 0x2,
            s_feature_ro_compat: 0x0,
            s_uuid: [0xAA; 16],
            s_nr_users: 1,
            s_dynsuper: 0,
            s_max_transaction: 0,
            s_max_trans_data: 0,
            s_checksum_type: 4,
            s_padding2: [0; 3],
            s_padding: [0xDEAD_BEEFu32; 42],
            s_checksum: 0xFEED_FACE,
            s_users: [0x55u8; 16 * 48],
        };

        let mut buf = [0u8; 1024];
        sb.to_disk_bytes(&mut buf);

        // spot check some fields are big-endian encoded
        assert_eq!(&buf[0..4], &JBD2_MAGIC.to_be_bytes());
        assert_eq!(&buf[0xC..0x10], &sb.s_blocksize.to_be_bytes());
        assert_eq!(&buf[0x10..0x14], &sb.s_maxlen.to_be_bytes());
        assert_eq!(&buf[0x14..0x18], &sb.s_first.to_be_bytes());
        assert_eq!(&buf[0x18..0x1C], &sb.s_sequence.to_be_bytes());
        assert_eq!(&buf[0x1C..0x20], &sb.s_start.to_be_bytes());
        assert_eq!(&buf[0xFC..0x100], &sb.s_checksum.to_be_bytes());

        let parsed = JournalSuperBlock::from_disk_bytes(&buf);
        assert_eq!(parsed.s_header.h_magic, sb.s_header.h_magic);
        assert_eq!(parsed.s_blocksize, sb.s_blocksize);
        assert_eq!(parsed.s_maxlen, sb.s_maxlen);
        assert_eq!(parsed.s_first, sb.s_first);
        assert_eq!(parsed.s_sequence, sb.s_sequence);
        assert_eq!(parsed.s_start, sb.s_start);
        assert_eq!(parsed.s_checksum, sb.s_checksum);
        assert_eq!(&parsed.s_users[..], &sb.s_users[..]);
    }

    #[test]
    fn checked_journal_superblock_codec_rejects_short_buffers() {
        for len in [0, 1023] {
            let bytes = alloc::vec![0u8; len];
            let error = JournalSuperBlock::decode_checked(&bytes)
                .expect_err("short journal superblock must be rejected");
            assert_eq!(error.kind(), crate::Ext4ErrorKind::Corrupted);
        }

        let superblock = JournalSuperBlock::default();
        let mut short = [0u8; 1023];
        let error = superblock
            .encode_checked(&mut short)
            .expect_err("short journal superblock output must be rejected");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Corrupted);
    }

    #[test]
    fn descriptor_capacity_accounts_for_linux_csum_v2_tag_padding_and_tail() {
        let mut superblock = JournalSuperBlock {
            s_feature_incompat: JBD2_FEATURE_INCOMPAT_CSUM_V2,
            s_checksum_type: JBD2_CRC32C_CHKSUM,
            ..Default::default()
        };
        assert_eq!(superblock.descriptor_tag_capacity(4096).unwrap(), 404);

        superblock.s_feature_incompat |= JBD2_FEATURE_INCOMPAT_64BIT;
        assert_eq!(superblock.descriptor_tag_capacity(4096).unwrap(), 289);
    }

    #[test]
    fn revoke_capacity_accounts_for_block_numbers_and_checksum_tail() {
        let mut superblock = JournalSuperBlock::default();
        assert_eq!(superblock.revoke_records_per_block(4096).unwrap(), 1020);

        superblock.s_feature_incompat = JBD2_FEATURE_INCOMPAT_64BIT | JBD2_FEATURE_INCOMPAT_CSUM_V3;
        superblock.s_checksum_type = JBD2_CRC32C_CHKSUM;
        assert_eq!(superblock.revoke_records_per_block(4096).unwrap(), 509);
    }

    #[test]
    fn checksum_mode_rejects_mixed_features_and_mismatched_types() {
        for feature_incompat in [
            JBD2_FEATURE_INCOMPAT_CSUM_V2 | JBD2_FEATURE_INCOMPAT_CSUM_V3,
            JBD2_FEATURE_INCOMPAT_CSUM_V2,
        ] {
            let mut superblock = JournalSuperBlock {
                s_feature_incompat: feature_incompat,
                ..JournalSuperBlock::default()
            };
            if feature_incompat == JBD2_FEATURE_INCOMPAT_CSUM_V2 {
                superblock.s_feature_compat = JBD2_FEATURE_COMPAT_CHECKSUM;
                superblock.s_checksum_type = JBD2_CRC32C_CHKSUM;
            }
            let error = superblock
                .checksum_mode()
                .expect_err("JBD2 checksum modes are mutually exclusive");
            assert_eq!(error.kind(), crate::Ext4ErrorKind::Unsupported);
        }

        let mut superblock = JournalSuperBlock {
            s_feature_incompat: JBD2_FEATURE_INCOMPAT_CSUM_V2,
            ..JournalSuperBlock::default()
        };
        let error = superblock
            .checksum_mode()
            .expect_err("CSUM_V2 requires the CRC32C superblock checksum type");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Unsupported);

        superblock.s_feature_incompat = 0;
        superblock.s_checksum_type = JBD2_CRC32C_CHKSUM;
        let error = superblock
            .checksum_mode()
            .expect_err("CRC32C checksum type requires CSUM_V2 or CSUM_V3");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Unsupported);
    }

    #[test]
    fn test_block_tag_and_tag3_roundtrip() {
        let tag = JournalBlockTagS {
            t_blocknr: 0xDEAD_BEEFu32,
            t_checksum: 0xABCDu16,
            t_flags: 0x0001,
        };
        let mut b = [0u8; 8];
        tag.to_disk_bytes(&mut b);
        assert_eq!(&b[0..4], &tag.t_blocknr.to_be_bytes());
        assert_eq!(&b[4..6], &tag.t_checksum.to_be_bytes());
        assert_eq!(&b[6..8], &tag.t_flags.to_be_bytes());
        let parsed = JournalBlockTagS::from_disk_bytes(&b);
        assert_eq!(parsed.t_blocknr, tag.t_blocknr);
        assert_eq!(parsed.t_checksum, tag.t_checksum);
        assert_eq!(parsed.t_flags, tag.t_flags);

        let tag3 = JournalBlockTag3S {
            t_blocknr: 1,
            t_flags: 2,
            t_blocknr_high: 3,
            t_checksum: 0xFEED_BEEFu32,
        };
        let mut b3 = [0u8; 16];
        tag3.to_disk_bytes(&mut b3);
        let parsed3 = JournalBlockTag3S::from_disk_bytes(&b3);
        assert_eq!(parsed3.t_blocknr, tag3.t_blocknr);
        assert_eq!(parsed3.t_flags, tag3.t_flags);
        assert_eq!(parsed3.t_blocknr_high, tag3.t_blocknr_high);
        assert_eq!(parsed3.t_checksum, tag3.t_checksum);
    }

    #[test]
    fn test_block_tail_and_revoke_roundtrip() {
        let tail = Jbd2JournalBlockTail {
            t_checksum: 0x1234_5678,
        };
        let mut b = [0u8; 4];
        tail.to_disk_bytes(&mut b);
        assert_eq!(&b[..], &tail.t_checksum.to_be_bytes());
        let parsed = Jbd2JournalBlockTail::from_disk_bytes(&b);
        assert_eq!(parsed.t_checksum, tail.t_checksum);

        let revoke = Jbd2JournalRevokeHeadS {
            r_header: JournalHeaderS {
                h_magic: JBD2_MAGIC,
                h_blocktype: 5,
                h_sequence: 7,
            },
            r_count: 16,
        };
        let mut rb = [0u8; 16];
        revoke.to_disk_bytes(&mut rb);
        let parsed_revoke = Jbd2JournalRevokeHeadS::from_disk_bytes(&rb);
        assert_eq!(parsed_revoke.r_header.h_magic, revoke.r_header.h_magic);
        assert_eq!(parsed_revoke.r_count, revoke.r_count);

        let rt = Jbd2JournalRevokeTail {
            r_checksum: 0xCAFEBABE,
        };
        let mut rtb = [0u8; 4];
        rt.to_disk_bytes(&mut rtb);
        let parsed_rt = Jbd2JournalRevokeTail::from_disk_bytes(&rtb);
        assert_eq!(parsed_rt.r_checksum, rt.r_checksum);
    }

    #[test]
    fn test_commit_header_roundtrip() {
        let hdr = JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: 2,
            h_sequence: 9,
        };
        let commit = CommitHeader {
            h_header: hdr,
            h_chksum_type: 4,
            h_chksum_size: 4,
            h_padding: [0u8; 2],
            h_chksum: [0x1111_2222u32; 8],
            h_commit_sec: 0x0102_0304_0506_0708u64,
            h_commit_nsec: 0xAABB_CCDDu32,
        };

        let mut buf = [0u8; 64];
        commit.to_disk_bytes(&mut buf);
        let parsed = CommitHeader::from_disk_bytes(&buf);
        assert_eq!(parsed.h_header.h_magic, commit.h_header.h_magic);
        assert_eq!(parsed.h_chksum_type, commit.h_chksum_type);
        assert_eq!(parsed.h_chksum_size, commit.h_chksum_size);
        assert_eq!(parsed.h_chksum, commit.h_chksum);
        assert_eq!(parsed.h_commit_sec, commit.h_commit_sec);
        assert_eq!(parsed.h_commit_nsec, commit.h_commit_nsec);
    }
}

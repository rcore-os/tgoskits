/// 注意，jbd2 全是大端序（on-disk values are big-endian）
use crate::ext4_backend::jbd2::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::superblock::*;
use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::entries::*;
use crate::ext4_backend::mkfile::*;
use crate::ext4_backend::*;
use crate::ext4_backend::datablock_cache::*;
use crate::ext4_backend::inodetable_cache::*;
use crate::ext4_backend::blockgroup_description::*;
use crate::ext4_backend::mkd::*;
use crate::ext4_backend::tool::*;
use crate::ext4_backend::jbd2::jbd2::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::bitmap::*;
use alloc::vec::{ Vec};
use alloc::vec;
use log::{error, trace};
use core::convert::TryInto;
pub const JOURNAL_FILE_INODE: u64 = 8; /// 根据 ext4 标准，journal 的 inode 为 8
pub const JBD2_MAGIC: u32 = 0xC03B_3998u32; // jbd2 magic number (on-disk big-endian)
pub const JOURNAL_BLOCK_COUNT:u32 = 32*1024*1024 /BLOCK_SIZE_U32;
pub const JOURANL_ESCAPE :u16 = 0x1;
pub const JBD2_FLAG_LAST_TAG:u16 = 0x8;
#[repr(C)]
///（主物理块号，元数据内容）
pub struct JBD2_UPDATE(pub u64,pub [u8;BLOCK_SIZE]);
pub const JBD2_BUFFER_MAX:usize=3;//最多3条缓存
#[repr(C)]
pub struct JBD2DEVSYSTEM{
    pub jbd2_super_block:journal_superblock_s,
    pub start_block:u32,// 日志区在磁盘的物理起始块号 include superblock
    pub max_len:u32,// 日志总块数
    pub head:u32,//当前日志写指针(块)(相对于 start_block 的偏移)
    pub sequence:u32, //下一个事务ID
    pub commit_queue:Vec<JBD2_UPDATE>, //事务缓存
}



#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct journal_header_s {
    pub h_magic: u32,      // __be32: magic number (0xC03B3998)
    pub h_blocktype: u32,  // __be32: block type (descriptor, commit, superblock, ...)
    pub h_sequence: u32,   // __be32: transaction sequence id
}
impl Default for journal_header_s {
    fn default() -> Self {
        journal_header_s { h_magic: JBD2_MAGIC, h_blocktype: 4,
             h_sequence: 0 }
    }
}

impl DiskFormat for journal_header_s {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let h_magic = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        let h_blocktype = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
        let h_sequence = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
        journal_header_s { h_magic, h_blocktype, h_sequence }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        bytes[0..4].copy_from_slice(&self.h_magic.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.h_blocktype.to_be_bytes());
        bytes[8..12].copy_from_slice(&self.h_sequence.to_be_bytes());
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct journal_superblock_s {
    // Offset 0x0 - 0xB: journal_header_t (12 bytes)
    pub s_header: journal_header_s,

    // Static information describing the journal
    pub s_blocksize: u32,       // 0xC  __be32
    pub s_maxlen: u32,          // 0x10 __be32: total number of blocks in journal
    pub s_first: u32,           // 0x14 __be32: first block of log information

    // Dynamic information describing the current state of the log
    pub s_sequence: u32,        // 0x18 __be32: first commit id expected in log
    pub s_start: u32,           // 0x1C __be32: block number of start of log
    pub s_errno: u32,           // 0x20 __be32: error value

    // The remaining fields are valid in a v2 superblock
    pub s_feature_compat: u32,      // 0x24 __be32
    pub s_feature_incompat: u32,    // 0x28 __be32
    pub s_feature_ro_compat: u32,   // 0x2C __be32
    pub s_uuid: [u8; 16],           // 0x30 __u8[16]
    pub s_nr_users: u32,            // 0x40 __be32
    pub s_dynsuper: u32,            // 0x44 __be32
    pub s_max_transaction: u32,     // 0x48 __be32
    pub s_max_trans_data: u32,      // 0x4C __be32
    pub s_checksum_type: u8,        // 0x50 __u8
    pub s_padding2: [u8; 3],        // 0x51 padding

    // padding up to 0xFC
    pub s_padding: [u32; 42],       // 0x54..0xFC
    pub s_checksum: u32,            // 0xFC __be32: checksum of superblock (with this zeroed)

    // 0x100 .. 0x3FF: list of users (16 * 48 = 768 bytes)
    pub s_users: [u8; 16 * 48],     // ids of filesystems sharing the log
}


impl Default for journal_superblock_s {
    ///必须手动配置max_len（块数）,默认4096个
    fn default() -> Self {
        let header = journal_header_s::default();
        journal_superblock_s { s_header: header, 
            s_blocksize: BLOCK_SIZE_U32, 
            s_maxlen: 4096, 
            s_first: 1,
             s_sequence: 1, 
             s_start: 0, 
             s_errno: 0,
              s_feature_compat: 0,
               s_feature_incompat: 0,
                s_feature_ro_compat: 0,
                 s_uuid: [0;16],
                  s_nr_users: 1, 
                  s_dynsuper: 0, 
                  s_max_transaction: JOURNAL_BLOCK_COUNT, 
                  s_max_trans_data: JOURNAL_BLOCK_COUNT*10, 
                  s_checksum_type: 0, 
                  s_padding2: [0;3], 
                  s_padding: [0;42], 
                  s_checksum: 0, 
                  s_users: [0;768] }
    }
}

impl DiskFormat for journal_superblock_s {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        // expect 1024 bytes
        let s_header = journal_header_s::from_disk_bytes(&bytes[0..12]);

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
        let mut s_padding2 = [0u8;3];
        s_padding2.copy_from_slice(&bytes[81..84]);

        let mut s_padding = [0u32; 42];
        let mut off = 84usize;
        for i in 0..42 {
            s_padding[i] = u32::from_be_bytes(bytes[off..off+4].try_into().unwrap());
            off += 4;
        }

        let s_checksum = u32::from_be_bytes(bytes[0xFC..0x100].try_into().unwrap());

        let mut s_users = [0u8; 16*48];
        s_users.copy_from_slice(&bytes[0x100..0x100 + 16*48]);

        journal_superblock_s{
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

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
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
        for i in 0..42 {
            bytes[off..off+4].copy_from_slice(&self.s_padding[i].to_be_bytes());
            off += 4;
        }

        bytes[0xFC..0x100].copy_from_slice(&self.s_checksum.to_be_bytes());
        bytes[0x100..0x100 + 16*48].copy_from_slice(&self.s_users);
    }
}

// Descriptor / Tag structures

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct journal_block_tag_s {
    // Basic (v1/v2) tag layout
    pub t_blocknr: u32,   // __be32: lower 32-bits of target block number
    pub t_checksum: u16,  // __be16: checksum (lower 16 bits)
    pub t_flags: u16,     // __be16: flags (escaped, same UUID, last tag, ...)
    // Optionally followed by __be32 t_blocknr_high (when 64-bit support)
    // and optionally a 16-byte uuid, depending on flags/features.
}

impl DiskFormat for journal_block_tag_s {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let t_blocknr = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        let t_checksum = u16::from_be_bytes(bytes[4..6].try_into().unwrap());
        let t_flags = u16::from_be_bytes(bytes[6..8].try_into().unwrap());
        journal_block_tag_s { t_blocknr, t_checksum, t_flags }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        bytes[0..4].copy_from_slice(&self.t_blocknr.to_be_bytes());
        bytes[4..6].copy_from_slice(&self.t_checksum.to_be_bytes());
        bytes[6..8].copy_from_slice(&self.t_flags.to_be_bytes());
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct journal_block_tag3_s {
    // v3 tag layout used when JBD2_FEATURE_INCOMPAT_CSUM_V3 is set
    pub t_blocknr: u32,      // __be32: lower 32 bits
    pub t_flags: u32,        // __be32: flags (includes LAST flag, SAME_UUID, ESCAPED)
    pub t_blocknr_high: u32, // __be32: upper 32 bits when 64-bit support present
    pub t_checksum: u32,     // __be32: full checksum
    // Optionally followed by a uuid (16 bytes) unless SAME_UUID flag set.
}

impl DiskFormat for journal_block_tag3_s {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let t_blocknr = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        let t_flags = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
        let t_blocknr_high = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
        let t_checksum = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
        journal_block_tag3_s { t_blocknr, t_flags, t_blocknr_high, t_checksum }
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
pub struct jbd2_journal_block_tail {
    pub t_checksum: u32, // __be32: checksum for descriptor block (with this zeroed)
}

impl DiskFormat for jbd2_journal_block_tail {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let t_checksum = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        jbd2_journal_block_tail { t_checksum }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        bytes[0..4].copy_from_slice(&self.t_checksum.to_be_bytes());
    }
}

// Revocation block header
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct jbd2_journal_revoke_header_s {
    pub r_header: journal_header_s, // common header
    pub r_count: u32,               // __be32: number of bytes used in this block
    // Followed by an array of block numbers (4 or 8 bytes each depending on 64-bit support)
}

impl DiskFormat for jbd2_journal_revoke_header_s {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let r_header = journal_header_s::from_disk_bytes(&bytes[0..12]);
        let r_count = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
        jbd2_journal_revoke_header_s { r_header, r_count }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        self.r_header.to_disk_bytes(&mut bytes[0..12]);
        bytes[12..16].copy_from_slice(&self.r_count.to_be_bytes());
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct jbd2_journal_revoke_tail {
    pub r_checksum: u32, // __be32: checksum of uuid + revoke block
}

impl DiskFormat for jbd2_journal_revoke_tail {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let r_checksum = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        jbd2_journal_revoke_tail { r_checksum }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        bytes[0..4].copy_from_slice(&self.r_checksum.to_be_bytes());
    }
}

// Commit block header
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct commit_header {
    pub h_header: journal_header_s, // common header (12 bytes)
    pub h_chksum_type: u8,          // 0xC  checksum type: 1=crc32,2=md5,3=sha1,4=crc32c
    pub h_chksum_size: u8,          // 0xD  size in bytes of checksum
    pub h_padding: [u8; 2],         // 0xE  padding
    pub h_chksum: [u32; 8],         // 0x10..0x2F: space for checksums (32 bytes)
    pub h_commit_sec: u64,          // 0x30 __be64: commit time seconds since epoch
    pub h_commit_nsec: u32,         // 0x38 __be32: commit time nanoseconds
}

#[cfg(test)]
mod tests {
    use super::*;
    use DiskFormat;

    #[test]
    fn test_journal_header_roundtrip() {
        let hdr = journal_header_s { h_magic: JBD2_MAGIC, h_blocktype: 2, h_sequence: 0x1122_3344 };
        let mut buf = [0u8; 12];
        hdr.to_disk_bytes(&mut buf);

        // Ensure big-endian ordering on disk
        assert_eq!(&buf[0..4], &JBD2_MAGIC.to_be_bytes());
        assert_eq!(&buf[4..8], &2u32.to_be_bytes());
        assert_eq!(&buf[8..12], &0x1122_3344u32.to_be_bytes());

        let parsed = journal_header_s::from_disk_bytes(&buf);
        assert_eq!(parsed.h_magic, JBD2_MAGIC);
        assert_eq!(parsed.h_blocktype, 2);
        assert_eq!(parsed.h_sequence, 0x1122_3344);
    }

    #[test]
    fn test_journal_superblock_roundtrip() {
        // build a sample superblock with distinct values
        let header = journal_header_s { h_magic: JBD2_MAGIC, h_blocktype: 3, h_sequence: 0xAABB_CCDD };
        let sb = journal_superblock_s {
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
            s_padding2: [0;3],
            s_padding: [0xDEAD_BEEFu32; 42],
            s_checksum: 0xFEED_FACE,
            s_users: [0x55u8; 16*48],
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

        let parsed = journal_superblock_s::from_disk_bytes(&buf);
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
    fn test_block_tag_and_tag3_roundtrip() {
        let tag = journal_block_tag_s { t_blocknr: 0xDEAD_BEEFu32, t_checksum: 0xABCDu16, t_flags: 0x0001 };
        let mut b = [0u8; 8];
        tag.to_disk_bytes(&mut b);
        assert_eq!(&b[0..4], &tag.t_blocknr.to_be_bytes());
        assert_eq!(&b[4..6], &tag.t_checksum.to_be_bytes());
        assert_eq!(&b[6..8], &tag.t_flags.to_be_bytes());
        let parsed = journal_block_tag_s::from_disk_bytes(&b);
        assert_eq!(parsed.t_blocknr, tag.t_blocknr);
        assert_eq!(parsed.t_checksum, tag.t_checksum);
        assert_eq!(parsed.t_flags, tag.t_flags);

        let tag3 = journal_block_tag3_s { t_blocknr: 1, t_flags: 2, t_blocknr_high: 3, t_checksum: 0xFEED_BEEFu32 };
        let mut b3 = [0u8; 16];
        tag3.to_disk_bytes(&mut b3);
        let parsed3 = journal_block_tag3_s::from_disk_bytes(&b3);
        assert_eq!(parsed3.t_blocknr, tag3.t_blocknr);
        assert_eq!(parsed3.t_flags, tag3.t_flags);
        assert_eq!(parsed3.t_blocknr_high, tag3.t_blocknr_high);
        assert_eq!(parsed3.t_checksum, tag3.t_checksum);
    }

    #[test]
    fn test_block_tail_and_revoke_roundtrip() {
        let tail = jbd2_journal_block_tail { t_checksum: 0x1234_5678 };
        let mut b = [0u8; 4];
        tail.to_disk_bytes(&mut b);
        assert_eq!(&b[..], &tail.t_checksum.to_be_bytes());
        let parsed = jbd2_journal_block_tail::from_disk_bytes(&b);
        assert_eq!(parsed.t_checksum, tail.t_checksum);

        let revoke = jbd2_journal_revoke_header_s { r_header: journal_header_s { h_magic: JBD2_MAGIC, h_blocktype: 5, h_sequence: 7 }, r_count: 16 };
        let mut rb = [0u8; 16];
        revoke.to_disk_bytes(&mut rb);
        let parsed_revoke = jbd2_journal_revoke_header_s::from_disk_bytes(&rb);
        assert_eq!(parsed_revoke.r_header.h_magic, revoke.r_header.h_magic);
        assert_eq!(parsed_revoke.r_count, revoke.r_count);

        let rt = jbd2_journal_revoke_tail { r_checksum: 0xCAFEBABE };
        let mut rtb = [0u8; 4];
        rt.to_disk_bytes(&mut rtb);
        let parsed_rt = jbd2_journal_revoke_tail::from_disk_bytes(&rtb);
        assert_eq!(parsed_rt.r_checksum, rt.r_checksum);
    }

    #[test]
    fn test_commit_header_roundtrip() {
        let hdr = journal_header_s { h_magic: JBD2_MAGIC, h_blocktype: 2, h_sequence: 9 };
        let commit = commit_header {
            h_header: hdr,
            h_chksum_type: 4,
            h_chksum_size: 4,
            h_padding: [0u8;2],
            h_chksum: [0x1111_2222u32; 8],
            h_commit_sec: 0x0102_0304_0506_0708u64,
            h_commit_nsec: 0xAABB_CCDDu32,
        };

        let mut buf = [0u8; 64];
        commit.to_disk_bytes(&mut buf);
        let parsed = commit_header::from_disk_bytes(&buf);
        assert_eq!(parsed.h_header.h_magic, commit.h_header.h_magic);
        assert_eq!(parsed.h_chksum_type, commit.h_chksum_type);
        assert_eq!(parsed.h_chksum_size, commit.h_chksum_size);
        assert_eq!(parsed.h_chksum, commit.h_chksum);
        assert_eq!(parsed.h_commit_sec, commit.h_commit_sec);
        assert_eq!(parsed.h_commit_nsec, commit.h_commit_nsec);
    }
}

impl DiskFormat for commit_header {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let h_header = journal_header_s::from_disk_bytes(&bytes[0..12]);
        let h_chksum_type = bytes[12];
        let h_chksum_size = bytes[13];
        let mut h_padding = [0u8;2];
        h_padding.copy_from_slice(&bytes[14..16]);

        let mut h_chksum = [0u32;8];
        let mut off = 16usize;
        for i in 0..8 {
            h_chksum[i] = u32::from_be_bytes(bytes[off..off+4].try_into().unwrap());
            off += 4;
        }

        let h_commit_sec = u64::from_be_bytes(bytes[48..56].try_into().unwrap());
        let h_commit_nsec = u32::from_be_bytes(bytes[56..60].try_into().unwrap());

        commit_header { h_header, h_chksum_type, h_chksum_size, h_padding, h_chksum, h_commit_sec, h_commit_nsec }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        self.h_header.to_disk_bytes(&mut bytes[0..12]);
        bytes[12] = self.h_chksum_type;
        bytes[13] = self.h_chksum_size;
        bytes[14..16].copy_from_slice(&self.h_padding);

        let mut off = 16usize;
        for i in 0..8 {
            bytes[off..off+4].copy_from_slice(&self.h_chksum[i].to_be_bytes());
            off += 4;
        }

        bytes[48..56].copy_from_slice(&self.h_commit_sec.to_be_bytes());
        bytes[56..60].copy_from_slice(&self.h_commit_nsec.to_be_bytes());
    }
}
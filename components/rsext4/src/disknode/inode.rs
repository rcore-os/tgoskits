use super::*;

/// On-disk ext4 inode layout.
///
/// This is the packed metadata record stored in the inode table. Every file,
/// directory, and symlink is described by one inode entry.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Ext4Inode {
    // 0x00: core POSIX metadata and 32-bit time fields.
    pub i_mode: u16,        // File type and permission bits.
    pub i_uid: u16,         // Low 16 bits of the owner UID.
    pub i_size_lo: u32,     // Low 32 bits of file size in bytes.
    pub i_atime: u32,       // Access time in seconds.
    pub i_ctime: u32,       // Status change time in seconds.
    pub i_mtime: u32,       // Modification time in seconds.
    pub i_dtime: u32,       // Deletion time in seconds.
    pub i_gid: u16,         // Low 16 bits of the owner GID.
    pub i_links_count: u16, // Hard-link count.
    pub i_blocks_lo: u32,   // Low 32 bits of the block count.
    pub i_flags: u32,       // ext4 inode flag bitset.

    // 0x20: Linux-specific versioning state.
    pub l_i_version: u32, // Linux inode version, used by NFS-style consumers.

    // 0x24: 60-byte block map area, reused as an extent tree root when enabled.
    pub i_block: [u32; 15], // Direct block words or embedded extent root.

    // 0x64: size/version/xattr continuation fields.
    pub i_generation: u32,  // Inode generation, commonly used by NFS.
    pub i_file_acl_lo: u32, // Low 32 bits of the xattr block pointer.
    pub i_size_high: u32,   // High 32 bits of file size or directory ACL.
    pub i_obso_faddr: u32,  // Obsolete fragment address field.

    // 0x74: Linux-specific high halves and checksum storage.
    pub l_i_blocks_high: u16,   // High 16 bits of the block count.
    pub l_i_file_acl_high: u16, // High 16 bits of the xattr block pointer.
    pub l_i_uid_high: u16,      // High 16 bits of the owner UID.
    pub l_i_gid_high: u16,      // High 16 bits of the owner GID.
    pub l_i_checksum_lo: u16,   // Low 16 bits of the inode checksum.
    pub l_i_reserved: u16,      // Reserved Linux field.

    // 0x80: extra inode area present in large inodes.
    pub i_extra_isize: u16, // Size of the extra inode payload beyond 128 bytes.
    pub i_checksum_hi: u16, // High 16 bits of the inode checksum.
    pub i_ctime_extra: u32, // ctime nanoseconds plus upper epoch bits.
    pub i_mtime_extra: u32, // mtime nanoseconds plus upper epoch bits.
    pub i_atime_extra: u32, // atime nanoseconds plus upper epoch bits.
    pub i_crtime: u32,      // Creation time in seconds.
    pub i_crtime_extra: u32, // crtime nanoseconds plus upper epoch bits.
    pub i_version_hi: u32,  // High 32 bits of the inode version.
    pub i_projid: u32,      // Project ID.
}

impl Ext4Inode {
    /// Initializes `i_block` with an empty embedded extent header.
    pub fn write_extend_header(&mut self) {
        let per_extent_header_offset = Ext4ExtentHeader::disk_size();
        let current_offset = 0;
        let mut extent_buffer: [u8; 60] = [0; 60];
        let header = Ext4ExtentHeader::new();
        // Serialize the header into a temporary byte buffer first so the packed
        // layout matches the on-disk representation exactly.
        header.to_disk_bytes(
            &mut extent_buffer[current_offset..current_offset + per_extent_header_offset],
        );
        // Then reinterpret the buffer as the 15 little-endian words stored in
        // `i_block`.
        let mut new_slice: [u32; 15] = [0; 15];
        for idx in 0..15 {
            new_slice[idx] = u32::from_le_bytes([
                extent_buffer[idx * 4],
                extent_buffer[idx * 4 + 1],
                extent_buffer[idx * 4 + 2],
                extent_buffer[idx * 4 + 3],
            ])
        }
        self.i_block.copy_from_slice(&new_slice);
    }

    /// Classic ext2/ext3 inode size in bytes.
    pub const GOOD_OLD_INODE_SIZE: u16 = 128;

    /// Default large-inode size used by modern ext4.
    pub const LARGE_INODE_SIZE: u16 = 256;

    /// Number of epoch-extension bits stored in `*_extra` timestamp fields.
    pub const EXT4_EPOCH_BITS: u32 = 2;
    /// Mask for the epoch-extension bits in `*_extra`.
    pub const EXT4_EPOCH_MASK: u32 = 0x3;
    /// Mask for the nanosecond payload in `*_extra`.
    pub const EXT4_NSEC_MASK: u32 = !Self::EXT4_EPOCH_MASK;

    /// End offset required to access `i_checksum_hi`.
    pub const FIELD_END_I_CHECKSUM_HI: u16 = 132;
    /// End offset required to access `i_ctime_extra`.
    pub const FIELD_END_I_CTIME_EXTRA: u16 = 136;
    /// End offset required to access `i_mtime_extra`.
    pub const FIELD_END_I_MTIME_EXTRA: u16 = 140;
    /// End offset required to access `i_atime_extra`.
    pub const FIELD_END_I_ATIME_EXTRA: u16 = 144;
    /// End offset required to access `i_crtime`.
    pub const FIELD_END_I_CRTIME: u16 = 148;
    /// End offset required to access `i_crtime_extra`.
    pub const FIELD_END_I_CRTIME_EXTRA: u16 = 152;
    /// End offset required to access `i_version_hi`.
    pub const FIELD_END_I_VERSION_HI: u16 = 156;
    /// End offset required to access `i_projid`.
    pub const FIELD_END_I_PROJID: u16 = 160;

    /// Returns the full 64-bit file size.
    pub fn size(&self) -> u64 {
        (self.i_size_high as u64) << 32 | self.i_size_lo as u64
    }

    /// Returns the full 48-bit block count.
    pub fn blocks_count(&self) -> u64 {
        (self.l_i_blocks_high as u64) << 32 | self.i_blocks_lo as u64
    }

    /// Returns the merged 32-bit UID.
    pub fn uid(&self) -> u32 {
        (self.l_i_uid_high as u32) << 16 | self.i_uid as u32
    }

    /// Returns the merged 32-bit GID.
    pub fn gid(&self) -> u32 {
        (self.l_i_gid_high as u32) << 16 | self.i_gid as u32
    }

    pub fn set_uid(&mut self, uid: u32) {
        self.i_uid = (uid & 0xFFFF) as u16;
        self.l_i_uid_high = ((uid >> 16) & 0xFFFF) as u16;
    }

    pub fn set_gid(&mut self, gid: u32) {
        self.i_gid = (gid & 0xFFFF) as u16;
        self.l_i_gid_high = ((gid >> 16) & 0xFFFF) as u16;
    }

    /// Returns the merged 48-bit extended-attribute block pointer.
    pub fn file_acl(&self) -> u64 {
        (self.l_i_file_acl_high as u64) << 32 | self.i_file_acl_lo as u64
    }

    /// Returns true when the inode type is directory.
    pub fn is_dir(&self) -> bool {
        self.i_mode & Self::S_IFMT == Self::S_IFDIR
    }

    /// Returns true when the inode type is regular file.
    pub fn is_file(&self) -> bool {
        self.i_mode & Self::S_IFMT == Self::S_IFREG
    }

    /// Returns true when the inode type is symbolic link.
    pub fn is_symlink(&self) -> bool {
        self.i_mode & Self::S_IFMT == Self::S_IFLNK
    }

    pub fn permissions(&self) -> u16 {
        self.i_mode & !Self::S_IFMT
    }

    pub fn set_mode_preserve_type(&mut self, mode: u16) {
        self.i_mode = (self.i_mode & Self::S_IFMT) | (mode & !Self::S_IFMT);
    }

    pub fn set_mode_full(&mut self, mode: u16) {
        self.i_mode = mode;
    }

    pub fn is_executable(&self) -> bool {
        self.i_mode & (Self::S_IXUSR | Self::S_IXGRP | Self::S_IXOTH) != 0
    }

    pub fn clear_setid_bits_for_content_change(&mut self) {
        self.i_mode &= !Self::S_ISUID;
        if self.is_executable() {
            self.i_mode &= !Self::S_ISGID;
        }
    }

    pub fn clear_setid_bits_for_chown(&mut self) {
        self.i_mode &= !(Self::S_ISUID | Self::S_ISGID);
    }

    /// Returns true when `i_block` is interpreted as an extent tree root.
    fn is_extent(&self) -> bool {
        self.i_flags & Self::EXT4_EXTENTS_FL != 0
    }
    /// Verifies both the extent flag and the embedded extent-header magic.
    pub fn have_extend_header_and_use_extend(&self) -> bool {
        if !Self::is_extent(self) {
            debug!("Inode not have extend flag!");
            return false;
        }

        let word0_le = self.i_block[0].to_le_bytes();
        let magic = u16::from_le_bytes([word0_le[0], word0_le[1]]);
        if magic == Ext4ExtentHeader::EXT4_EXT_MAGIC {
            true
        } else {
            debug!("No tree header!!!");
            false
        }
    }

    // some metadata change support
    pub fn set_mtime(&mut self, mtime: u32) {
        self.i_mtime = mtime;
    }
    pub fn set_ctime(&mut self, ctime: u32) {
        self.i_ctime = ctime;
    }
    pub fn set_atime(&mut self, atime: u32) {
        self.i_atime = atime;
    }

    pub fn max_extra_isize(inode_size: u16) -> u16 {
        inode_size.saturating_sub(Self::GOOD_OLD_INODE_SIZE)
    }

    pub fn required_extra_isize(field_end: u16) -> u16 {
        field_end.saturating_sub(Self::GOOD_OLD_INODE_SIZE)
    }

    pub fn field_fits(&self, inode_size: u16, field_end: u16) -> bool {
        field_end <= inode_size && field_end <= Self::GOOD_OLD_INODE_SIZE + self.i_extra_isize
    }

    fn encode_extra_time(ts: Ext4Timestamp) -> u32 {
        let ts = Ext4Timestamp::new(ts.sec, ts.nsec);
        let lower = ts.sec as i32 as i64;
        let extra = (((ts.sec - lower) >> 32) as u32) & Self::EXT4_EPOCH_MASK;
        extra | (ts.nsec << Self::EXT4_EPOCH_BITS)
    }

    fn decode_extra_time(base: u32, extra: u32) -> Ext4Timestamp {
        let mut sec = (base as i32) as i64;
        if (extra & Self::EXT4_EPOCH_MASK) != 0 {
            sec += ((extra & Self::EXT4_EPOCH_MASK) as i64) << 32;
        }
        let nsec = (extra & Self::EXT4_NSEC_MASK) >> Self::EXT4_EPOCH_BITS;
        Ext4Timestamp::new(sec, nsec)
    }

    fn encode_time_base(ts: Ext4Timestamp, fits_extra: bool) -> u32 {
        let sec = if fits_extra {
            ts.sec
        } else {
            ts.sec.clamp(i32::MIN as i64, i32::MAX as i64)
        };
        (sec as i32) as u32
    }

    fn get_raw_xtime(
        &self,
        inode_size: u16,
        field_end: u16,
        sec_field: u32,
        extra_field: u32,
    ) -> Ext4Timestamp {
        if self.field_fits(inode_size, field_end) {
            Self::decode_extra_time(sec_field, extra_field)
        } else {
            Ext4Timestamp::new((sec_field as i32) as i64, 0)
        }
    }

    pub fn set_atime_ts(&mut self, inode_size: u16, ts: Ext4Timestamp) {
        let fits_extra = self.field_fits(inode_size, Self::FIELD_END_I_ATIME_EXTRA);
        self.i_atime = Self::encode_time_base(ts, fits_extra);
        self.i_atime_extra = if fits_extra {
            Self::encode_extra_time(ts)
        } else {
            0
        };
    }

    pub fn set_mtime_ts(&mut self, inode_size: u16, ts: Ext4Timestamp) {
        let fits_extra = self.field_fits(inode_size, Self::FIELD_END_I_MTIME_EXTRA);
        self.i_mtime = Self::encode_time_base(ts, fits_extra);
        self.i_mtime_extra = if fits_extra {
            Self::encode_extra_time(ts)
        } else {
            0
        };
    }

    pub fn set_ctime_ts(&mut self, inode_size: u16, ts: Ext4Timestamp) {
        let fits_extra = self.field_fits(inode_size, Self::FIELD_END_I_CTIME_EXTRA);
        self.i_ctime = Self::encode_time_base(ts, fits_extra);
        self.i_ctime_extra = if fits_extra {
            Self::encode_extra_time(ts)
        } else {
            0
        };
    }

    pub fn set_crtime_ts(&mut self, inode_size: u16, ts: Ext4Timestamp) {
        if !self.field_fits(inode_size, Self::FIELD_END_I_CRTIME) {
            self.i_crtime = 0;
            self.i_crtime_extra = 0;
            return;
        }

        self.i_crtime = Self::encode_time_base(
            ts,
            self.field_fits(inode_size, Self::FIELD_END_I_CRTIME_EXTRA),
        );
        self.i_crtime_extra = if self.field_fits(inode_size, Self::FIELD_END_I_CRTIME_EXTRA) {
            Self::encode_extra_time(ts)
        } else {
            0
        };
    }

    pub fn atime_ts(&self, inode_size: u16) -> Ext4Timestamp {
        self.get_raw_xtime(
            inode_size,
            Self::FIELD_END_I_ATIME_EXTRA,
            self.i_atime,
            self.i_atime_extra,
        )
    }

    pub fn mtime_ts(&self, inode_size: u16) -> Ext4Timestamp {
        self.get_raw_xtime(
            inode_size,
            Self::FIELD_END_I_MTIME_EXTRA,
            self.i_mtime,
            self.i_mtime_extra,
        )
    }

    pub fn ctime_ts(&self, inode_size: u16) -> Ext4Timestamp {
        self.get_raw_xtime(
            inode_size,
            Self::FIELD_END_I_CTIME_EXTRA,
            self.i_ctime,
            self.i_ctime_extra,
        )
    }

    pub fn crtime_ts(&self, inode_size: u16) -> Option<Ext4Timestamp> {
        if !self.field_fits(inode_size, Self::FIELD_END_I_CRTIME) {
            return None;
        }

        Some(
            if self.field_fits(inode_size, Self::FIELD_END_I_CRTIME_EXTRA) {
                Self::decode_extra_time(self.i_crtime, self.i_crtime_extra)
            } else {
                Ext4Timestamp::new((self.i_crtime as i32) as i64, 0)
            },
        )
    }

    pub fn empty_for_reuse(default_extra_isize: u16) -> Self {
        Self {
            i_extra_isize: default_extra_isize,
            ..Default::default()
        }
    }
}

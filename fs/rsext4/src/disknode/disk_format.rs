use super::*;

impl DiskFormat for Ext4ExtentHeader {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        Self {
            eh_magic: read_u16_le(&bytes[0..2]),
            eh_entries: read_u16_le(&bytes[2..4]),
            eh_max: read_u16_le(&bytes[4..6]),
            eh_depth: read_u16_le(&bytes[6..8]),
            eh_generation: read_u32_le(&bytes[8..12]),
        }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        write_u16_le(self.eh_magic, &mut bytes[0..2]);
        write_u16_le(self.eh_entries, &mut bytes[2..4]);
        write_u16_le(self.eh_max, &mut bytes[4..6]);
        write_u16_le(self.eh_depth, &mut bytes[6..8]);
        write_u32_le(self.eh_generation, &mut bytes[8..12]);
    }

    fn disk_size() -> usize {
        12
    }
}

impl DiskFormat for Ext4ExtentIdx {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        Self {
            ei_block: read_u32_le(&bytes[0..4]),
            ei_leaf_lo: read_u32_le(&bytes[4..8]),
            ei_leaf_hi: read_u16_le(&bytes[8..10]),
            ei_unused: read_u16_le(&bytes[10..12]),
        }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        write_u32_le(self.ei_block, &mut bytes[0..4]);
        write_u32_le(self.ei_leaf_lo, &mut bytes[4..8]);
        write_u16_le(self.ei_leaf_hi, &mut bytes[8..10]);
        write_u16_le(self.ei_unused, &mut bytes[10..12]);
    }

    fn disk_size() -> usize {
        12
    }
}

impl DiskFormat for Ext4Extent {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        Self {
            ee_block: read_u32_le(&bytes[0..4]),
            ee_len: read_u16_le(&bytes[4..6]),
            ee_start_hi: read_u16_le(&bytes[6..8]),
            ee_start_lo: read_u32_le(&bytes[8..12]),
        }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        write_u32_le(self.ee_block, &mut bytes[0..4]);
        write_u16_le(self.ee_len, &mut bytes[4..6]);
        write_u16_le(self.ee_start_hi, &mut bytes[6..8]);
        write_u32_le(self.ee_start_lo, &mut bytes[8..12]);
    }

    fn disk_size() -> usize {
        12
    }
}

impl DiskFormat for Ext4Inode {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        let mut inode = Self {
            i_mode: read_u16_le(&bytes[0..2]),
            i_uid: read_u16_le(&bytes[2..4]),
            i_size_lo: read_u32_le(&bytes[4..8]),
            i_atime: read_u32_le(&bytes[8..12]),
            i_ctime: read_u32_le(&bytes[12..16]),
            i_mtime: read_u32_le(&bytes[16..20]),
            i_dtime: read_u32_le(&bytes[20..24]),
            i_gid: read_u16_le(&bytes[24..26]),
            i_links_count: read_u16_le(&bytes[26..28]),
            i_blocks_lo: read_u32_le(&bytes[28..32]),
            i_flags: read_u32_le(&bytes[32..36]),
            l_i_version: read_u32_le(&bytes[36..40]),
            i_block: [0; 15],
            i_generation: read_u32_le(&bytes[100..104]),
            i_file_acl_lo: read_u32_le(&bytes[104..108]),
            i_size_high: read_u32_le(&bytes[108..112]),
            i_obso_faddr: read_u32_le(&bytes[112..116]),
            l_i_blocks_high: read_u16_le(&bytes[116..118]),
            l_i_file_acl_high: read_u16_le(&bytes[118..120]),
            l_i_uid_high: read_u16_le(&bytes[120..122]),
            l_i_gid_high: read_u16_le(&bytes[122..124]),
            l_i_checksum_lo: read_u16_le(&bytes[124..126]),
            l_i_reserved: read_u16_le(&bytes[126..128]),
            i_extra_isize: 0,
            i_checksum_hi: 0,
            i_ctime_extra: 0,
            i_mtime_extra: 0,
            i_atime_extra: 0,
            i_crtime: 0,
            i_crtime_extra: 0,
            i_version_hi: 0,
            i_projid: 0,
        };

        for i in 0..15 {
            inode.i_block[i] = read_u32_le(&bytes[40 + i * 4..44 + i * 4]);
        }

        if bytes.len() >= 256 {
            inode.i_extra_isize = read_u16_le(&bytes[128..130]);
            inode.i_checksum_hi = read_u16_le(&bytes[130..132]);
            inode.i_ctime_extra = read_u32_le(&bytes[132..136]);
            inode.i_mtime_extra = read_u32_le(&bytes[136..140]);
            inode.i_atime_extra = read_u32_le(&bytes[140..144]);
            inode.i_crtime = read_u32_le(&bytes[144..148]);
            inode.i_crtime_extra = read_u32_le(&bytes[148..152]);
            inode.i_version_hi = read_u32_le(&bytes[152..156]);
            inode.i_projid = read_u32_le(&bytes[156..160]);
        }

        inode
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        write_u16_le(self.i_mode, &mut bytes[0..2]);
        write_u16_le(self.i_uid, &mut bytes[2..4]);
        write_u32_le(self.i_size_lo, &mut bytes[4..8]);
        write_u32_le(self.i_atime, &mut bytes[8..12]);
        write_u32_le(self.i_ctime, &mut bytes[12..16]);
        write_u32_le(self.i_mtime, &mut bytes[16..20]);
        write_u32_le(self.i_dtime, &mut bytes[20..24]);
        write_u16_le(self.i_gid, &mut bytes[24..26]);
        write_u16_le(self.i_links_count, &mut bytes[26..28]);
        write_u32_le(self.i_blocks_lo, &mut bytes[28..32]);
        write_u32_le(self.i_flags, &mut bytes[32..36]);
        write_u32_le(self.l_i_version, &mut bytes[36..40]);

        for i in 0..15 {
            write_u32_le(self.i_block[i], &mut bytes[40 + i * 4..44 + i * 4]);
        }

        write_u32_le(self.i_generation, &mut bytes[100..104]);
        write_u32_le(self.i_file_acl_lo, &mut bytes[104..108]);
        write_u32_le(self.i_size_high, &mut bytes[108..112]);
        write_u32_le(self.i_obso_faddr, &mut bytes[112..116]);
        write_u16_le(self.l_i_blocks_high, &mut bytes[116..118]);
        write_u16_le(self.l_i_file_acl_high, &mut bytes[118..120]);
        write_u16_le(self.l_i_uid_high, &mut bytes[120..122]);
        write_u16_le(self.l_i_gid_high, &mut bytes[122..124]);
        write_u16_le(self.l_i_checksum_lo, &mut bytes[124..126]);
        write_u16_le(self.l_i_reserved, &mut bytes[126..128]);

        if bytes.len() >= 256 {
            write_u16_le(self.i_extra_isize, &mut bytes[128..130]);
            write_u16_le(self.i_checksum_hi, &mut bytes[130..132]);
            write_u32_le(self.i_ctime_extra, &mut bytes[132..136]);
            write_u32_le(self.i_mtime_extra, &mut bytes[136..140]);
            write_u32_le(self.i_atime_extra, &mut bytes[140..144]);
            write_u32_le(self.i_crtime, &mut bytes[144..148]);
            write_u32_le(self.i_crtime_extra, &mut bytes[148..152]);
            write_u32_le(self.i_version_hi, &mut bytes[152..156]);
            write_u32_le(self.i_projid, &mut bytes[156..160]);
        }
    }

    fn disk_size() -> usize {
        Self::GOOD_OLD_INODE_SIZE as usize
    }
}

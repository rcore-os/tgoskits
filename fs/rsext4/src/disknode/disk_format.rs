use super::*;
use crate::error::{Ext4Error, Ext4Result};

impl Ext4Inode {
    /// Decodes an inode record after validating its fixed and extra regions.
    pub(crate) fn decode_checked(bytes: &[u8]) -> Ext4Result<Self> {
        let base_size = usize::from(Self::GOOD_OLD_INODE_SIZE);
        if bytes.len() < base_size || u16::try_from(bytes.len()).is_err() {
            return Err(Ext4Error::corrupted().with_operation("inode:decode_size"));
        }

        if bytes.len() > base_size {
            let extra_isize = bytes
                .get(base_size..base_size + 2)
                .map(read_u16_le)
                .ok_or_else(|| Ext4Error::corrupted().with_operation("inode:decode_extra_isize"))?;
            let extra_end = base_size + usize::from(extra_isize);
            if !extra_isize.is_multiple_of(4) || extra_end > bytes.len() {
                return Err(Ext4Error::corrupted().with_operation("inode:decode_extra_isize"));
            }
        }

        Ok(Self::from_disk_bytes(bytes))
    }
}

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

        if bytes.len() >= 130 {
            inode.i_extra_isize = read_u16_le(&bytes[128..130]);
        }
        let inode_size = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
        if inode.field_fits(inode_size, Self::FIELD_END_I_CHECKSUM_HI) {
            inode.i_checksum_hi = read_u16_le(&bytes[130..132]);
        }
        if inode.field_fits(inode_size, Self::FIELD_END_I_CTIME_EXTRA) {
            inode.i_ctime_extra = read_u32_le(&bytes[132..136]);
        }
        if inode.field_fits(inode_size, Self::FIELD_END_I_MTIME_EXTRA) {
            inode.i_mtime_extra = read_u32_le(&bytes[136..140]);
        }
        if inode.field_fits(inode_size, Self::FIELD_END_I_ATIME_EXTRA) {
            inode.i_atime_extra = read_u32_le(&bytes[140..144]);
        }
        if inode.field_fits(inode_size, Self::FIELD_END_I_CRTIME) {
            inode.i_crtime = read_u32_le(&bytes[144..148]);
        }
        if inode.field_fits(inode_size, Self::FIELD_END_I_CRTIME_EXTRA) {
            inode.i_crtime_extra = read_u32_le(&bytes[148..152]);
        }
        if inode.field_fits(inode_size, Self::FIELD_END_I_VERSION_HI) {
            inode.i_version_hi = read_u32_le(&bytes[152..156]);
        }
        if inode.field_fits(inode_size, Self::FIELD_END_I_PROJID) {
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

        if bytes.len() >= 130 {
            write_u16_le(self.i_extra_isize, &mut bytes[128..130]);
        }
        let inode_size = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
        if self.field_fits(inode_size, Self::FIELD_END_I_CHECKSUM_HI) {
            write_u16_le(self.i_checksum_hi, &mut bytes[130..132]);
        }
        if self.field_fits(inode_size, Self::FIELD_END_I_CTIME_EXTRA) {
            write_u32_le(self.i_ctime_extra, &mut bytes[132..136]);
        }
        if self.field_fits(inode_size, Self::FIELD_END_I_MTIME_EXTRA) {
            write_u32_le(self.i_mtime_extra, &mut bytes[136..140]);
        }
        if self.field_fits(inode_size, Self::FIELD_END_I_ATIME_EXTRA) {
            write_u32_le(self.i_atime_extra, &mut bytes[140..144]);
        }
        if self.field_fits(inode_size, Self::FIELD_END_I_CRTIME) {
            write_u32_le(self.i_crtime, &mut bytes[144..148]);
        }
        if self.field_fits(inode_size, Self::FIELD_END_I_CRTIME_EXTRA) {
            write_u32_le(self.i_crtime_extra, &mut bytes[148..152]);
        }
        if self.field_fits(inode_size, Self::FIELD_END_I_VERSION_HI) {
            write_u32_le(self.i_version_hi, &mut bytes[152..156]);
        }
        if self.field_fits(inode_size, Self::FIELD_END_I_PROJID) {
            write_u32_le(self.i_projid, &mut bytes[156..160]);
        }
    }

    fn disk_size() -> usize {
        Self::GOOD_OLD_INODE_SIZE as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_inode_codec_rejects_short_and_invalid_extra_inode_bytes() {
        for raw in [alloc::vec![], alloc::vec![0; 127], alloc::vec![0; 129]] {
            let error = Ext4Inode::decode_checked(&raw).expect_err("short inode must be rejected");
            assert_eq!(error.kind(), crate::Ext4ErrorKind::Corrupted);
        }

        let mut oversized_extra = alloc::vec![0; Ext4Inode::LARGE_INODE_SIZE as usize];
        oversized_extra[128..130].copy_from_slice(&132_u16.to_le_bytes());
        let error = Ext4Inode::decode_checked(&oversized_extra)
            .expect_err("extra inode bytes must fit the inode record");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Corrupted);

        let mut unaligned_extra = alloc::vec![0; Ext4Inode::LARGE_INODE_SIZE as usize];
        unaligned_extra[128..130].copy_from_slice(&6_u16.to_le_bytes());
        let error = Ext4Inode::decode_checked(&unaligned_extra)
            .expect_err("extra inode bytes must be four-byte aligned");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Corrupted);
    }

    #[test]
    fn inode_codec_preserves_fields_beyond_extra_isize() {
        // Test idea: serializing a parsed inode must not overwrite the inline
        // xattr region with zero-valued fields that i_extra_isize excludes.
        let mut raw = [0xa5; Ext4Inode::LARGE_INODE_SIZE as usize];
        raw[..Ext4Inode::GOOD_OLD_INODE_SIZE as usize].fill(0);
        raw[128..130].copy_from_slice(&4u16.to_le_bytes());
        raw[130..132].copy_from_slice(&0x1234u16.to_le_bytes());
        let original_tail = raw[132..].to_vec();

        let mut inode = Ext4Inode::from_disk_bytes(&raw);
        assert_eq!(inode.i_checksum_hi, 0x1234);
        assert_eq!(inode.i_ctime_extra, 0);
        inode.i_mtime = 99;
        inode.to_disk_bytes(&mut raw);

        assert_eq!(u32::from_le_bytes(raw[16..20].try_into().unwrap()), 99);
        assert_eq!(&raw[132..], original_tail.as_slice());
    }
}

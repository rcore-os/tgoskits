//! Disk serialization for directory entry headers.

use super::{Ext4DirEntry2, Ext4DirEntryTail};
use crate::{config::*, endian::*};

impl DiskFormat for Ext4DirEntry2 {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        Self {
            inode: read_u32_le(&bytes[0..4]),
            rec_len: read_u16_le(&bytes[4..6]),
            name_len: bytes[6],
            file_type: bytes[7],
            name: [0; DIRNAME_LEN],
        }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        write_u32_le(self.inode, &mut bytes[0..4]);
        write_u16_le(self.rec_len, &mut bytes[4..6]);
        bytes[6] = self.name_len;
        bytes[7] = self.file_type;
    }

    fn disk_size() -> usize {
        8
    }
}

impl DiskFormat for Ext4DirEntryTail {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        Self {
            det_reserved_zero1: read_u32_le(&bytes[0..4]),
            det_rec_len: read_u16_le(&bytes[4..6]),
            det_reserved_zero2: bytes[6],
            det_reserved_ft: bytes[7],
            det_checksum: read_u32_le(&bytes[8..12]),
        }
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        write_u32_le(self.det_reserved_zero1, &mut bytes[0..4]);
        write_u16_le(self.det_rec_len, &mut bytes[4..6]);
        bytes[6] = self.det_reserved_zero2;
        bytes[7] = self.det_reserved_ft;
        write_u32_le(self.det_checksum, &mut bytes[8..12]);
    }

    fn disk_size() -> usize {
        Ext4DirEntryTail::TAIL_LEN as usize
    }
}

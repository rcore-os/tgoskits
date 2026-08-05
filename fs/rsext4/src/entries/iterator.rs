//! Directory entry parsing and iteration helpers.

/// Parsed directory entry view.
#[derive(Debug)]
pub struct Ext4DirEntryInfo<'a> {
    pub inode: u32,
    pub file_type: u8,
    pub name: &'a [u8],
}

impl<'a> Ext4DirEntryInfo<'a> {
    /// Parses an ext4 directory entry from raw bytes.
    pub fn parse_from_bytes(data: &'a [u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }

        let inode = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if inode == 0 {
            return None;
        }

        let rec_len = u16::from_le_bytes([data[4], data[5]]);
        let name_len = data[6] as usize;
        let file_type = data[7];

        if rec_len < 8 || name_len > 255 || data.len() < 8 + name_len {
            return None;
        }

        let name = &data[8..8 + name_len];

        Some(Ext4DirEntryInfo {
            inode,
            file_type,
            name,
        })
    }

    /// Returns the entry name as UTF-8 when valid.
    pub fn name_str(&self) -> Option<&str> {
        core::str::from_utf8(self.name).ok()
    }

    /// Returns whether the entry is `"."`.
    pub fn is_dot(&self) -> bool {
        self.name == b"."
    }

    /// Returns whether the entry is `".."`.
    pub fn is_dotdot(&self) -> bool {
        self.name == b".."
    }
}

/// Iterator over directory entries in a directory block.
pub struct DirEntryIterator<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> DirEntryIterator<'a> {
    /// Creates a new iterator over a directory block.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
}

impl<'a> Iterator for DirEntryIterator<'a> {
    type Item = (Ext4DirEntryInfo<'a>, u16);

    fn next(&mut self) -> Option<Self::Item> {
        while self.offset < self.data.len() {
            let remaining = &self.data[self.offset..];
            if remaining.len() < 8 {
                return None;
            }

            let rec_len = u16::from_le_bytes([remaining[4], remaining[5]]);
            if rec_len < 8 || rec_len as usize > remaining.len() {
                return None;
            }

            let entry_data = &remaining[..rec_len as usize];
            self.offset += rec_len as usize;

            if let Some(entry_info) = Ext4DirEntryInfo::parse_from_bytes(entry_data) {
                return Some((entry_info, rec_len));
            }
        }

        None
    }
}

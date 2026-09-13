//! Directory inspection and resumable linear/indexed cursors.

use super::*;

/// Stable directory-entry type independent from VFS or Linux ABI enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryEntryType {
    Unknown,
    RegularFile,
    Directory,
    CharacterDevice,
    BlockDevice,
    Fifo,
    Socket,
    Symlink,
}

/// Special inode kind accepted by the portable core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialInodeKind {
    CharacterDevice(DeviceNumber),
    BlockDevice(DeviceNumber),
    Fifo,
    Socket,
}

impl SpecialInodeKind {
    pub(super) const fn inode_type(self) -> u16 {
        match self {
            Self::CharacterDevice(_) => Ext4Inode::S_IFCHR,
            Self::BlockDevice(_) => Ext4Inode::S_IFBLK,
            Self::Fifo => Ext4Inode::S_IFIFO,
            Self::Socket => Ext4Inode::S_IFSOCK,
        }
    }

    pub(super) const fn directory_entry_type(self) -> u8 {
        match self {
            Self::CharacterDevice(_) => Ext4DirEntry2::EXT4_FT_CHRDEV,
            Self::BlockDevice(_) => Ext4DirEntry2::EXT4_FT_BLKDEV,
            Self::Fifo => Ext4DirEntry2::EXT4_FT_FIFO,
            Self::Socket => Ext4DirEntry2::EXT4_FT_SOCK,
        }
    }

    pub(super) const fn payload(self) -> CreateInodePayload<'static> {
        match self {
            Self::CharacterDevice(device) | Self::BlockDevice(device) => {
                CreateInodePayload::Device(device)
            }
            Self::Fifo | Self::Socket => CreateInodePayload::Empty,
        }
    }
}

impl DirectoryEntryType {
    pub(super) fn from_disk(value: u8) -> Ext4Result<Self> {
        match value {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::RegularFile),
            2 => Ok(Self::Directory),
            3 => Ok(Self::CharacterDevice),
            4 => Ok(Self::BlockDevice),
            5 => Ok(Self::Fifo),
            6 => Ok(Self::Socket),
            7 => Ok(Self::Symlink),
            _ => Err(Ext4Error::corrupted().with_operation("directory:file_type")),
        }
    }
}

/// One directory record returned by [`Ext4::read_directory`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub inode: InodeNumber,
    pub file_type: DirectoryEntryType,
    pub name: Vec<u8>,
    /// Cursor of the next record.
    pub next_cursor: DirectoryCursor,
}

/// Opaque core cursor used to resume directory enumeration.
///
/// Linear directories use byte offsets. Indexed directories use the complete
/// ext4 hash plus a collision ordinal that is deliberately not compressed into
/// a Linux ABI cookie by the OS-independent core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryCursor {
    /// Begin enumeration from the first visible record.
    Start,
    /// Resume a linear directory at an on-disk byte offset.
    Linear { offset: u64 },
    /// Resume an indexed directory at a hash and exact-collision ordinal.
    HTree {
        major: u32,
        minor: u32,
        collision: u32,
    },
    /// Enumeration has reached end of directory.
    End,
}

/// Per-open directory state owned by an embedding VFS open description.
///
/// The representation stays private so HTree paths and cached directory
/// records never become part of the portable core API. The caller-provided
/// [`DirectoryCursor`] remains the authoritative position: this state is a
/// discardable acceleration cache and does not need transactional rollback
/// when an I/O or copy-to-user operation fails.
#[derive(Debug)]
pub struct DirectoryReader {
    directory: InodeNumber,
    indexed: Option<IndexedDirectoryReader>,
}

#[derive(Debug)]
struct IndexedDirectoryReader {
    change_attribute: u64,
    ranges: VecDeque<IndexedDirectoryRange>,
}

impl DirectoryReader {
    pub(super) fn new(directory: InodeNumber) -> Self {
        Self {
            directory,
            indexed: None,
        }
    }

    pub const fn directory(&self) -> InodeNumber {
        self.directory
    }

    /// Discards parsed HTree ranges after an external seek or policy change.
    pub fn reset(&mut self) {
        self.indexed = None;
    }
}

fn indexed_cursor_key(cursor: DirectoryCursor) -> Ext4Result<(u32, u32, u32)> {
    match cursor {
        DirectoryCursor::Start => Ok((0, 0, 0)),
        DirectoryCursor::HTree {
            major,
            minor,
            collision,
        } if major & 1 == 0 => Ok((major, minor, collision)),
        DirectoryCursor::HTree { .. } => {
            Err(Ext4Error::invalid_input().with_operation("directory:indexed_hash_cursor"))
        }
        DirectoryCursor::Linear { .. } => {
            Err(Ext4Error::invalid_input().with_operation("directory:indexed_cursor"))
        }
        DirectoryCursor::End => {
            Err(Ext4Error::invalid_input().with_operation("directory:indexed_end_cursor"))
        }
    }
}

const fn indexed_record_key(record: &IndexedDirectoryRecord) -> (u32, u32, u32) {
    (record.major, record.minor, record.collision)
}

const fn indexed_key_cursor((major, minor, collision): (u32, u32, u32)) -> DirectoryCursor {
    DirectoryCursor::HTree {
        major,
        minor,
        collision,
    }
}

fn indexed_range_position(range: &IndexedDirectoryRange, cursor: (u32, u32, u32)) -> Option<usize> {
    if range.start == cursor {
        return Some(0);
    }
    range
        .records
        .iter()
        .position(|record| indexed_record_key(record) == cursor)
}

fn indexed_record_count(ranges: &VecDeque<IndexedDirectoryRange>, first_record: usize) -> usize {
    ranges
        .iter()
        .enumerate()
        .map(|(index, range)| {
            if index == 0 {
                range.records.len().saturating_sub(first_record)
            } else {
                range.records.len()
            }
        })
        .sum()
}

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    /// Looks up one raw child name without performing path traversal.
    pub fn lookup_child(
        &mut self,
        parent: InodeNumber,
        name: FileName<'_>,
    ) -> Ext4Result<Option<InodeInfo>> {
        let Some(number) = self.lookup_child_number(parent, name)? else {
            return Ok(None);
        };
        let inode = self.filesystem.get_inode_by_num(&mut self.device, number)?;
        self.inspect_inode(number, inode).map(Some)
    }

    /// Resolves a directory record without loading the child inode table.
    /// The embedding VFS must acquire the returned inode's allocation reference
    /// before releasing namespace/mount exclusion. A bare number is not a pin.
    pub fn lookup_child_number(
        &mut self,
        parent: InodeNumber,
        name: FileName<'_>,
    ) -> Ext4Result<Option<InodeNumber>> {
        let parent_inode = self.filesystem.get_inode_by_num(&mut self.device, parent)?;
        let entry = match find_named_entry_in_parent(
            &mut self.filesystem,
            &mut self.device,
            parent,
            &parent_inode,
            name.as_bytes(),
        ) {
            Ok(entry) => entry,
            Err(error) if error.kind() == Ext4ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(entry.ino))
    }

    /// Reads directory records from a core-owned cursor.
    ///
    /// Deleted records and checksum tails advance the cookie but are not
    /// returned. Malformed records and checksum mismatches are corruption,
    /// never an implicit end-of-directory.
    pub fn read_directory(
        &mut self,
        directory: InodeNumber,
        cursor: DirectoryCursor,
        max_entries: usize,
    ) -> Ext4Result<Vec<DirectoryEntry>> {
        let mut reader = DirectoryReader::new(directory);
        self.read_directory_with_reader(&mut reader, cursor, max_entries)
    }

    /// Opens private directory-enumeration state for one VFS open description.
    pub fn open_directory_reader(&mut self, directory: InodeNumber) -> Ext4Result<DirectoryReader> {
        let inode = self
            .filesystem
            .get_inode_by_num(&mut self.device, directory)?;
        if !inode.is_dir() {
            return Err(Ext4Error::not_dir());
        }
        Ok(DirectoryReader::new(directory))
    }

    /// Reads directory records while retaining discardable per-open HTree
    /// range state.
    ///
    /// `cursor` is the only authoritative enumeration position. Callers may
    /// retry the same cursor after an error even if this method populated or
    /// discarded cached ranges before returning the error.
    pub fn read_directory_with_reader(
        &mut self,
        reader: &mut DirectoryReader,
        cursor: DirectoryCursor,
        max_entries: usize,
    ) -> Ext4Result<Vec<DirectoryEntry>> {
        let directory = reader.directory;
        let mut inode = self
            .filesystem
            .get_inode_by_num(&mut self.device, directory)?;
        if !inode.is_dir() {
            return Err(Ext4Error::not_dir());
        }
        if max_entries == 0 || cursor == DirectoryCursor::End {
            return Ok(Vec::new());
        }
        if inode.is_htree_indexed() {
            return self.read_indexed_directory_with_reader(reader, &inode, cursor, max_entries);
        }
        reader.indexed = None;

        let offset = match cursor {
            DirectoryCursor::Start => 0,
            DirectoryCursor::Linear { offset } => offset,
            DirectoryCursor::HTree { .. } => {
                return Err(Ext4Error::invalid_input().with_operation("directory:linear_cursor"));
            }
            DirectoryCursor::End => return Ok(Vec::new()),
        };
        if offset >= self.filesystem.inode_size(&inode) {
            return Ok(Vec::new());
        }

        let block_size = self.filesystem.block_size();
        let mappings = resolve_inode_blocks(
            &mut self.filesystem,
            &mut self.device,
            directory,
            &mut inode,
        )?;
        let mut output = Vec::new();
        for (logical_block, physical_block) in mappings {
            let block_base = u64::from(logical_block)
                .checked_mul(block_size as u64)
                .ok_or_else(Ext4Error::overflow)?;
            let block_end = block_base
                .checked_add(block_size as u64)
                .ok_or_else(Ext4Error::overflow)?;
            if block_end <= offset {
                continue;
            }

            let cached = self
                .filesystem
                .datablock_cache
                .get_or_load(&mut self.device, physical_block)?;
            let data = &cached.data;
            let checksum_ok = if inode.is_htree_indexed() {
                verify_ext4_dx_checksum(
                    &self.filesystem.superblock,
                    directory.raw(),
                    inode.i_generation,
                    data,
                )
                .unwrap_or_else(|| {
                    verify_ext4_dirblock_checksum(
                        &self.filesystem.superblock,
                        directory.raw(),
                        inode.i_generation,
                        data,
                    )
                })
            } else {
                verify_ext4_dirblock_checksum(
                    &self.filesystem.superblock,
                    directory.raw(),
                    inode.i_generation,
                    data,
                )
            };
            if !checksum_ok {
                return Err(Ext4Error::checksum().with_operation("directory:block"));
            }

            let mut position = 0usize;
            while position < data.len() {
                let header = data.get(position..position + 8).ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("directory:record_header")
                })?;
                let inode_raw = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
                let record_len = usize::from(u16::from_le_bytes([header[4], header[5]]));
                let name_len = usize::from(header[6]);
                let file_type = header[7];
                let record_end = position.checked_add(record_len).ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("directory:record_overflow")
                })?;
                if record_len < 8
                    || !record_len.is_multiple_of(4)
                    || record_end > data.len()
                    || name_len > record_len - 8
                {
                    return Err(Ext4Error::corrupted().with_operation("directory:record"));
                }

                let entry_offset = block_base
                    .checked_add(position as u64)
                    .ok_or_else(Ext4Error::overflow)?;
                let next_offset = block_base
                    .checked_add(record_end as u64)
                    .ok_or_else(Ext4Error::overflow)?;
                if inode_raw != 0 && entry_offset >= offset {
                    let inode_number = InodeNumber::new(inode_raw)
                        .map_err(|_| Ext4Error::corrupted().with_operation("directory:inode"))?;
                    let name = data[position + 8..position + 8 + name_len].to_vec();
                    FileName::new(&name).map_err(|_| {
                        Ext4Error::corrupted().with_operation("directory:stored_name")
                    })?;
                    output.push(DirectoryEntry {
                        inode: inode_number,
                        file_type: DirectoryEntryType::from_disk(file_type)?,
                        name,
                        next_cursor: DirectoryCursor::Linear {
                            offset: next_offset,
                        },
                    });
                    if output.len() == max_entries {
                        return Ok(output);
                    }
                }
                position = record_end;
            }
        }
        Ok(output)
    }

    pub(super) fn read_indexed_directory_with_reader(
        &mut self,
        reader: &mut DirectoryReader,
        inode: &Ext4Inode,
        cursor: DirectoryCursor,
        max_entries: usize,
    ) -> Ext4Result<Vec<DirectoryEntry>> {
        let start = indexed_cursor_key(cursor)?;
        let change_attribute = inode.version(self.filesystem.inode_disk_size());
        match &mut reader.indexed {
            Some(indexed) if indexed.change_attribute == change_attribute => {}
            Some(indexed) => {
                indexed.change_attribute = change_attribute;
                indexed.ranges.clear();
            }
            None => {
                reader.indexed = Some(IndexedDirectoryReader {
                    change_attribute,
                    ranges: VecDeque::new(),
                });
            }
        }

        let range_index = reader.indexed.as_ref().and_then(|indexed| {
            indexed
                .ranges
                .iter()
                .position(|range| indexed_range_position(range, start).is_some())
        });
        let range_index = match range_index {
            Some(index) => index,
            None => {
                let range = self.load_indexed_directory_range(reader.directory, inode, start)?;
                let indexed = reader.indexed.as_mut().ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("directory:reader_state")
                })?;
                indexed.ranges.clear();
                indexed.ranges.push_back(range);
                0
            }
        };
        let indexed = reader
            .indexed
            .as_mut()
            .ok_or_else(|| Ext4Error::corrupted().with_operation("directory:reader_state"))?;
        for _ in 0..range_index {
            let _ = indexed.ranges.pop_front();
        }

        let record_index = indexed
            .ranges
            .front()
            .and_then(|range| indexed_range_position(range, start))
            .ok_or_else(|| Ext4Error::corrupted().with_operation("directory:reader_cursor"))?;
        let lookahead = max_entries.checked_add(1).unwrap_or(max_entries);
        while indexed_record_count(&indexed.ranges, record_index) < lookahead {
            let Some(next_start) = indexed.ranges.back().and_then(|range| range.next_start) else {
                break;
            };
            let previous_start = indexed
                .ranges
                .back()
                .map(|range| range.start)
                .ok_or_else(|| Ext4Error::corrupted().with_operation("directory:reader_range"))?;
            if next_start <= previous_start
                || indexed.ranges.iter().any(|range| range.start == next_start)
            {
                return Err(Ext4Error::corrupted().with_operation("directory:reader_cycle"));
            }
            let range = self.load_indexed_directory_range(reader.directory, inode, next_start)?;
            indexed.ranges.push_back(range);
        }

        let mut records = Vec::with_capacity(lookahead.min(128));
        for (range_index, range) in indexed.ranges.iter().enumerate() {
            let first = if range_index == 0 { record_index } else { 0 };
            for record in range.records.iter().skip(first) {
                records.push(record);
                if records.len() == lookahead {
                    break;
                }
            }
            if records.len() == lookahead {
                break;
            }
        }

        let returned = records.len().min(max_entries);
        let mut output = Vec::with_capacity(returned);
        for index in 0..returned {
            let record = records[index];
            let next_cursor = records
                .get(index + 1)
                .map(|record| indexed_key_cursor(indexed_record_key(record)))
                .unwrap_or(DirectoryCursor::End);
            output.push(DirectoryEntry {
                inode: InodeNumber::new(record.inode).map_err(|_| {
                    Ext4Error::corrupted().with_operation("directory:indexed_inode")
                })?,
                file_type: DirectoryEntryType::from_disk(record.file_type)?,
                name: record.name.clone(),
                next_cursor,
            });
        }
        Ok(output)
    }

    pub(super) fn load_indexed_directory_range(
        &mut self,
        directory: InodeNumber,
        inode: &Ext4Inode,
        start: (u32, u32, u32),
    ) -> Ext4Result<IndexedDirectoryRange> {
        read_indexed_directory_range(
            &mut self.filesystem,
            &mut self.device,
            directory,
            inode,
            start,
        )
    }

    /// Returns the terminal cursor for directory seek semantics.
    ///
    /// Linear directories use their byte size. HTree directories use an
    /// opaque terminal state so an OS adapter can encode the architecture's
    /// ext4 EOF cookie without leaking ABI policy into the portable core.
    pub fn directory_end_cursor(&mut self, directory: InodeNumber) -> Ext4Result<DirectoryCursor> {
        let inode = self
            .filesystem
            .get_inode_by_num(&mut self.device, directory)?;
        if !inode.is_dir() {
            return Err(Ext4Error::not_dir());
        }
        if inode.is_htree_indexed() {
            Ok(DirectoryCursor::End)
        } else {
            Ok(DirectoryCursor::Linear {
                offset: self.filesystem.inode_size(&inode),
            })
        }
    }
}

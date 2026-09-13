//! Extended-attribute on-disk decoding and typed core operations.

use alloc::vec::Vec;

use crate::{
    BlockIo, Ext4FileSystem, Jbd2Dev,
    blockdev::TransactionCredits,
    bmalloc::{AbsoluteBN, BGIndex, InodeNumber},
    cache::bitmap::CacheKey,
    checksum::ext4_metadata_csum32,
    crc32c::{ext4_crc32c_seed_from_superblock, ext4_superblock_has_metadata_csum},
    disknode::Ext4Inode,
    endian::{read_u16_le, read_u32_le, write_u16_le, write_u32_le},
    error::{Ext4Error, Ext4ErrorKind, Ext4Result},
    runtime::Clock,
    superblock::Ext4Superblock,
};

pub(crate) const XATTR_MAGIC: u32 = 0xea02_0000;
pub(crate) const XATTR_IBODY_HEADER_SIZE: usize = 4;
const XATTR_BLOCK_HEADER_SIZE: usize = 32;
const XATTR_ENTRY_SIZE: usize = 16;
const XATTR_TERMINATOR_SIZE: usize = 4;
const XATTR_SIZE_MAX: u32 = 1 << 24;
const XATTR_REFCOUNT_MAX: u32 = 1024;
const XATTR_BLOCK_CHECKSUM_OFFSET: usize = 16;
const XATTR_TRANSACTION_CREDITS: usize = 8;

/// Namespace encoded by an ext4 xattr entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum XattrNamespace {
    User,
    PosixAclAccess,
    PosixAclDefault,
    Trusted,
    Lustre,
    Security,
    System,
    RichAcl,
    Encryption,
    Hurd,
    Other(u8),
}

impl XattrNamespace {
    pub const fn from_disk_index(index: u8) -> Self {
        match index {
            1 => Self::User,
            2 => Self::PosixAclAccess,
            3 => Self::PosixAclDefault,
            4 => Self::Trusted,
            5 => Self::Lustre,
            6 => Self::Security,
            7 => Self::System,
            8 => Self::RichAcl,
            9 => Self::Encryption,
            10 => Self::Hurd,
            index => Self::Other(index),
        }
    }

    pub const fn disk_index(self) -> u8 {
        match self {
            Self::User => 1,
            Self::PosixAclAccess => 2,
            Self::PosixAclDefault => 3,
            Self::Trusted => 4,
            Self::Lustre => 5,
            Self::Security => 6,
            Self::System => 7,
            Self::RichAcl => 8,
            Self::Encryption => 9,
            Self::Hurd => 10,
            Self::Other(index) => index,
        }
    }
}

/// One raw xattr name returned by the portable core.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct XattrName {
    pub namespace: XattrNamespace,
    pub name: Vec<u8>,
}

/// Linux-compatible create/replace policy without exposing syscall flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XattrSetMode {
    Upsert,
    Create,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StoredXattrValue {
    Local(Vec<u8>),
    EaInode { inode: u32, size: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredXattr {
    name: XattrName,
    value: StoredXattrValue,
}

pub(super) struct ExternalStore {
    block: AbsoluteBN,
    refcount: u32,
    entries: Vec<StoredXattr>,
    raw: Vec<u8>,
}

struct XattrLayout {
    inline: Vec<StoredXattr>,
    external: Vec<StoredXattr>,
}

struct XattrPersistence<'a> {
    inode_number: InodeNumber,
    original_inode: &'a Ext4Inode,
    layout: XattrLayout,
    external_store: Option<ExternalStore>,
    transaction_credits: usize,
}

pub(crate) fn get_inode_xattr<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    filesystem: &mut Ext4FileSystem,
    inode_number: InodeNumber,
    namespace: XattrNamespace,
    name: &[u8],
) -> Ext4Result<Vec<u8>> {
    validate_name(name)?;
    let (inode, raw_inode) = load_allocated_inode(device, filesystem, inode_number)?;

    if let Some(entries) = parse_inline_store(filesystem, &inode, &raw_inode)?
        && let Some(value) = find_value(entries, namespace, name)?
    {
        return Ok(value);
    }
    if let Some(store) = read_external_store(device, filesystem, &inode)?
        && let Some(value) = find_value(store.entries, namespace, name)?
    {
        return Ok(value);
    }
    Err(Ext4Error::not_found().with_operation("xattr:get"))
}

pub(crate) fn list_inode_xattrs<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    filesystem: &mut Ext4FileSystem,
    inode_number: InodeNumber,
) -> Ext4Result<Vec<XattrName>> {
    let (inode, raw_inode) = load_allocated_inode(device, filesystem, inode_number)?;
    let mut names = Vec::new();
    if let Some(entries) = parse_inline_store(filesystem, &inode, &raw_inode)? {
        names.extend(entries.into_iter().map(|entry| entry.name));
    }
    if let Some(store) = read_external_store(device, filesystem, &inode)? {
        names.extend(store.entries.into_iter().map(|entry| entry.name));
    }
    Ok(names)
}

pub(crate) fn set_inode_xattr<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    filesystem: &mut Ext4FileSystem,
    inode_number: InodeNumber,
    namespace: XattrNamespace,
    name: &[u8],
    value: &[u8],
    mode: XattrSetMode,
) -> Ext4Result<()> {
    validate_name(name)?;
    if value.len() > XATTR_SIZE_MAX as usize {
        return Err(Ext4Error::invalid_input().with_operation("xattr:value_size"));
    }
    let (inode, raw_inode) = load_allocated_inode(device, filesystem, inode_number)?;
    let mut layout = XattrLayout {
        inline: parse_inline_store(filesystem, &inode, &raw_inode)?.unwrap_or_default(),
        external: Vec::new(),
    };
    let external_store = read_external_store(device, filesystem, &inode)?;
    layout.external = external_store
        .as_ref()
        .map_or_else(Vec::new, |store| store.entries.clone());
    let inline_index = find_entry(&layout.inline, namespace, name);
    let external_index = find_entry(&layout.external, namespace, name);
    let existing = inline_index.is_some() || external_index.is_some();
    match (mode, existing) {
        (XattrSetMode::Create, true) => {
            return Err(Ext4Error::already_exists().with_operation("xattr:create"));
        }
        (XattrSetMode::Replace, false) => {
            return Err(Ext4Error::not_found().with_operation("xattr:replace"));
        }
        _ => {}
    }
    let replacement = StoredXattr {
        name: XattrName {
            namespace,
            name: name.to_vec(),
        },
        value: StoredXattrValue::Local(value.to_vec()),
    };
    if stored_value_matches(&layout.inline, inline_index, &replacement)
        || stored_value_matches(&layout.external, external_index, &replacement)
    {
        return Ok(());
    }

    let mut inline_candidate = layout.inline.clone();
    replace_or_insert(&mut inline_candidate, inline_index, replacement.clone());
    match encode_inline_xattrs(filesystem, &inode, &mut inline_candidate) {
        Ok(_) => {
            layout.inline = inline_candidate;
            remove_entry(&mut layout.external, namespace, name);
        }
        Err(error) if error.kind() == Ext4ErrorKind::NoSpace => {
            replace_or_insert(&mut layout.external, external_index, replacement);
            let mut external_candidate = layout.external.clone();
            encode_external_xattrs(filesystem, &mut external_candidate)?;
            layout.external = external_candidate;
            remove_entry(&mut layout.inline, namespace, name);
        }
        Err(error) => return Err(error),
    }
    persist_xattrs(
        device,
        filesystem,
        XattrPersistence {
            inode_number,
            original_inode: &inode,
            layout,
            external_store,
            transaction_credits: XATTR_TRANSACTION_CREDITS,
        },
    )
}

fn find_entry(entries: &[StoredXattr], namespace: XattrNamespace, name: &[u8]) -> Option<usize> {
    entries
        .iter()
        .position(|entry| entry.name.namespace == namespace && entry.name.name.as_slice() == name)
}

fn stored_value_matches(
    entries: &[StoredXattr],
    index: Option<usize>,
    replacement: &StoredXattr,
) -> bool {
    index.is_some_and(|index| entries[index].value == replacement.value)
}

fn replace_or_insert(
    entries: &mut Vec<StoredXattr>,
    index: Option<usize>,
    replacement: StoredXattr,
) {
    if let Some(index) = index {
        entries[index] = replacement;
    } else {
        entries.push(replacement);
    }
}

fn remove_entry(entries: &mut Vec<StoredXattr>, namespace: XattrNamespace, name: &[u8]) -> bool {
    if let Some(index) = find_entry(entries, namespace, name) {
        entries.remove(index);
        true
    } else {
        false
    }
}

fn encode_inline_xattrs(
    filesystem: &Ext4FileSystem,
    inode: &Ext4Inode,
    entries: &mut [StoredXattr],
) -> Ext4Result<Vec<u8>> {
    let inode_size = filesystem.inode_disk_size() as usize;
    let inline_offset = usize::from(Ext4Inode::GOOD_OLD_INODE_SIZE)
        .checked_add(usize::from(inode.i_extra_isize))
        .ok_or_else(Ext4Error::overflow)?;
    let storage_size = inode_size
        .checked_sub(inline_offset)
        .ok_or_else(|| Ext4Error::no_space().with_operation("xattr:inline_bounds"))?;
    encode_xattrs(
        entries,
        storage_size,
        XATTR_IBODY_HEADER_SIZE,
        XATTR_IBODY_HEADER_SIZE,
        false,
    )
}

fn encode_external_xattrs(
    filesystem: &Ext4FileSystem,
    entries: &mut [StoredXattr],
) -> Ext4Result<Vec<u8>> {
    encode_xattrs(
        entries,
        filesystem.block_size(),
        XATTR_BLOCK_HEADER_SIZE,
        0,
        true,
    )
}

pub(crate) fn remove_inode_xattr<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    filesystem: &mut Ext4FileSystem,
    inode_number: InodeNumber,
    namespace: XattrNamespace,
    name: &[u8],
) -> Ext4Result<()> {
    validate_name(name)?;
    let (inode, raw_inode) = load_allocated_inode(device, filesystem, inode_number)?;
    let mut layout = XattrLayout {
        inline: parse_inline_store(filesystem, &inode, &raw_inode)?.unwrap_or_default(),
        external: Vec::new(),
    };
    let external_store = read_external_store(device, filesystem, &inode)?;
    layout.external = external_store
        .as_ref()
        .map_or_else(Vec::new, |store| store.entries.clone());
    if !remove_entry(&mut layout.inline, namespace, name)
        && !remove_entry(&mut layout.external, namespace, name)
    {
        return Err(Ext4Error::not_found().with_operation("xattr:remove"));
    }
    persist_xattrs(
        device,
        filesystem,
        XattrPersistence {
            inode_number,
            original_inode: &inode,
            layout,
            external_store,
            transaction_credits: XATTR_TRANSACTION_CREDITS,
        },
    )
}

fn persist_xattrs<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    filesystem: &mut Ext4FileSystem,
    persistence: XattrPersistence<'_>,
) -> Ext4Result<()> {
    let XattrPersistence {
        inode_number,
        original_inode,
        mut layout,
        external_store,
        transaction_credits,
    } = persistence;
    let inode_size = filesystem.inode_disk_size() as usize;
    let inline_offset = usize::from(Ext4Inode::GOOD_OLD_INODE_SIZE)
        .checked_add(usize::from(original_inode.i_extra_isize))
        .ok_or_else(Ext4Error::overflow)?;
    let inline_image = if inline_offset <= inode_size {
        Some(encode_inline_xattrs(
            filesystem,
            original_inode,
            &mut layout.inline,
        )?)
    } else if layout.inline.is_empty() {
        None
    } else {
        return Err(Ext4Error::no_space().with_operation("xattr:inline_bounds"));
    };
    let mut external_image = if layout.external.is_empty() {
        None
    } else {
        Some(encode_external_xattrs(filesystem, &mut layout.external)?)
    };
    let external_unchanged = external_store
        .as_ref()
        .is_some_and(|store| store.entries == layout.external);
    let timestamp = device.now()?;
    let ctime = timestamp.sec.clamp(0, i64::from(u32::MAX)) as u32;
    let block_size = filesystem.block_size() as u32;
    let huge_file = filesystem
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);

    let feature_was_missing =
        filesystem.superblock.s_feature_compat & Ext4Superblock::EXT4_FEATURE_COMPAT_EXT_ATTR == 0;
    let revoke_records = usize::from(
        external_image.is_none()
            && external_store
                .as_ref()
                .is_some_and(|store| store.refcount == 1),
    );
    let credits = TransactionCredits::metadata_with_revokes(transaction_credits, revoke_records);
    filesystem.with_metadata_transaction(device, credits, |filesystem, device| {
        let mut allocated_block = None;
        let mut touched_bitmap_groups = Vec::<BGIndex>::new();
        let mut superblock_dirty = feature_was_missing;
        let target_block = if external_unchanged {
            external_store.as_ref().map(|store| store.block)
        } else if let Some(image) = external_image.as_mut() {
            let block = match external_store.as_ref() {
                Some(store) if store.refcount == 1 => store.block,
                Some(store) if store.refcount == 0 || store.refcount > 1024 => {
                    return Err(Ext4Error::corrupted().with_operation("xattr:block_refcount"));
                }
                _ => {
                    let block = filesystem.alloc_block(device)?;
                    allocated_block = Some(block);
                    let (group, _) = filesystem.block_allocator.global_to_group(block)?;
                    touched_bitmap_groups.push(group);
                    superblock_dirty = true;
                    block
                }
            };
            set_external_block_checksum(&filesystem.superblock, block, image)?;
            device.write_blocks(image, block, 1, true)?;
            Some(block)
        } else {
            None
        };

        let inode_update =
            filesystem.modify_inode_record(device, inode_number, |inode, raw_inode| {
                if inline_offset > raw_inode.len() {
                    return Err(Ext4Error::corrupted().with_operation("xattr:inode_bounds"));
                }
                let inline_tail = &mut raw_inode[inline_offset..];
                inline_tail.fill(0);
                if let Some(image) = &inline_image {
                    if image.len() != inline_tail.len() {
                        return Err(Ext4Error::corrupted().with_operation("xattr:inline_size"));
                    }
                    inline_tail.copy_from_slice(image);
                }

                let had_external = inode.file_acl() != 0;
                let has_external = target_block.is_some();
                let mut sectors = inode.blocks_count(block_size, huge_file);
                let xattr_sectors = u64::from(block_size / 512);
                match (had_external, has_external) {
                    (false, true) => {
                        sectors = sectors
                            .checked_add(xattr_sectors)
                            .ok_or_else(Ext4Error::overflow)?;
                    }
                    (true, false) => {
                        sectors = sectors.checked_sub(xattr_sectors).ok_or_else(|| {
                            Ext4Error::corrupted().with_operation("xattr:i_blocks")
                        })?;
                    }
                    _ => {}
                }
                inode.set_blocks_count(sectors, block_size, huge_file)?;
                inode.set_file_acl(target_block.map_or(0, AbsoluteBN::raw))?;
                inode.i_ctime = ctime;
                inode.increment_version(u16::try_from(raw_inode.len()).unwrap_or(u16::MAX));
                Ok(())
            });
        if let Err(error) = inode_update {
            if let Some(block) = allocated_block {
                device.forget_unpublished_metadata(block);
                filesystem.free_block(device, block)?;
            }
            return Err(error);
        }

        if let Some(old) = external_store
            && Some(old.block) != target_block
            && let Some(group) = release_external_store(device, filesystem, old)?
        {
            if !touched_bitmap_groups.contains(&group) {
                touched_bitmap_groups.push(group);
            }
            superblock_dirty = true;
        }
        filesystem.superblock.s_feature_compat |= Ext4Superblock::EXT4_FEATURE_COMPAT_EXT_ATTR;
        if feature_was_missing {
            filesystem.mark_superblock_dirty();
        }

        // Materialize every metadata image while the same bounded handle is
        // active. A later filesystem-wide sync must not split one xattr state
        // transition across journal transactions.
        filesystem.inodetable_cache.flush(device, inode_number)?;
        for group in touched_bitmap_groups {
            filesystem
                .bitmap_cache
                .flush(device, &CacheKey::new_block(group))?;
            filesystem.sync_group_descriptor(device, group)?;
        }
        if superblock_dirty {
            filesystem.sync_superblock(device)?;
        }
        Ok(())
    })
}

fn encode_xattrs(
    entries: &mut [StoredXattr],
    storage_size: usize,
    entries_start: usize,
    value_base: usize,
    external: bool,
) -> Ext4Result<Vec<u8>> {
    if storage_size < entries_start + XATTR_TERMINATOR_SIZE {
        return Err(Ext4Error::no_space().with_operation("xattr:encode"));
    }
    if external {
        entries.sort_by(|left, right| {
            left.name
                .namespace
                .disk_index()
                .cmp(&right.name.namespace.disk_index())
                .then(left.name.name.len().cmp(&right.name.name.len()))
                .then(left.name.name.cmp(&right.name.name))
        });
    }

    let mut names_end = entries_start;
    for entry in entries.iter() {
        validate_name(&entry.name.name)?;
        names_end = names_end
            .checked_add(round_up_4(
                XATTR_ENTRY_SIZE
                    .checked_add(entry.name.name.len())
                    .ok_or_else(Ext4Error::overflow)?,
            )?)
            .ok_or_else(Ext4Error::overflow)?;
    }
    names_end = names_end
        .checked_add(XATTR_TERMINATOR_SIZE)
        .ok_or_else(Ext4Error::overflow)?;
    if names_end > storage_size {
        return Err(Ext4Error::no_space().with_operation("xattr:encode_names"));
    }

    let mut image = alloc::vec![0u8; storage_size];
    if entries.is_empty() {
        return Ok(image);
    }
    write_u32_le(XATTR_MAGIC, &mut image[0..4]);
    if external {
        write_u32_le(1, &mut image[4..8]);
        write_u32_le(1, &mut image[8..12]);
    }

    let mut entry_offset = entries_start;
    let mut value_cursor = storage_size;
    let mut entry_hashes = Vec::with_capacity(entries.len());
    for entry in entries.iter() {
        let value = match &entry.value {
            StoredXattrValue::Local(value) => value,
            StoredXattrValue::EaInode { .. } => {
                return Err(Ext4Error::unsupported_feature(
                    crate::error::FeatureSet::Incompatible,
                    Ext4Superblock::EXT4_FEATURE_INCOMPAT_EA_INODE,
                )
                .with_operation("xattr:encode_ea_inode"));
            }
        };
        let padded_value_size = round_up_4(value.len())?;
        value_cursor = value_cursor
            .checked_sub(padded_value_size)
            .ok_or_else(|| Ext4Error::no_space().with_operation("xattr:encode_value"))?;
        if value_cursor < names_end || value_cursor < value_base {
            return Err(Ext4Error::no_space().with_operation("xattr:encode_value"));
        }
        let value_offset = value_cursor - value_base;
        let value_offset = u16::try_from(value_offset)
            .map_err(|_| Ext4Error::no_space().with_operation("xattr:value_offset"))?;
        let value_size = u32::try_from(value.len())
            .map_err(|_| Ext4Error::invalid_input().with_operation("xattr:value_size"))?;

        let entry_end = entry_offset + XATTR_ENTRY_SIZE;
        image[entry_offset] = u8::try_from(entry.name.name.len())
            .map_err(|_| Ext4Error::invalid_input().with_operation("xattr:name"))?;
        image[entry_offset + 1] = entry.name.namespace.disk_index();
        if !value.is_empty() {
            write_u16_le(value_offset, &mut image[entry_offset + 2..entry_offset + 4]);
            image[value_cursor..value_cursor + value.len()].copy_from_slice(value);
        }
        write_u32_le(value_size, &mut image[entry_offset + 8..entry_offset + 12]);
        image[entry_end..entry_end + entry.name.name.len()].copy_from_slice(&entry.name.name);

        let hash = if external {
            xattr_entry_hash(
                &entry.name.name,
                &image[value_cursor..value_cursor + padded_value_size],
            )
        } else {
            0
        };
        write_u32_le(hash, &mut image[entry_offset + 12..entry_offset + 16]);
        entry_hashes.push(hash);
        entry_offset += round_up_4(XATTR_ENTRY_SIZE + entry.name.name.len())?;
    }

    if external {
        let mut block_hash = 0u32;
        for hash in entry_hashes {
            if hash == 0 {
                block_hash = 0;
                break;
            }
            block_hash = block_hash.rotate_left(16) ^ hash;
        }
        write_u32_le(block_hash, &mut image[12..16]);
    }
    Ok(image)
}

fn xattr_entry_hash(name: &[u8], padded_value: &[u8]) -> u32 {
    let mut hash = 0u32;
    for byte in name {
        hash = hash.rotate_left(5) ^ u32::from(*byte);
    }
    for word in padded_value.as_chunks::<4>().0 {
        hash = hash.rotate_left(16) ^ read_u32_le(word);
    }
    hash
}

fn set_external_block_checksum(
    superblock: &Ext4Superblock,
    block_number: AbsoluteBN,
    block: &mut [u8],
) -> Ext4Result<()> {
    if block.len() < XATTR_BLOCK_HEADER_SIZE {
        return Err(Ext4Error::corrupted().with_operation("xattr:block_checksum_bounds"));
    }
    block[XATTR_BLOCK_CHECKSUM_OFFSET..XATTR_BLOCK_CHECKSUM_OFFSET + 4].fill(0);
    if ext4_superblock_has_metadata_csum(superblock) {
        let block_number = block_number.raw().to_le_bytes();
        let checksum = ext4_metadata_csum32(
            ext4_crc32c_seed_from_superblock(superblock),
            &[&block_number, block],
        );
        write_u32_le(
            checksum,
            &mut block[XATTR_BLOCK_CHECKSUM_OFFSET..XATTR_BLOCK_CHECKSUM_OFFSET + 4],
        );
    }
    Ok(())
}

fn load_allocated_inode<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    filesystem: &mut Ext4FileSystem,
    inode_number: InodeNumber,
) -> Ext4Result<(Ext4Inode, Vec<u8>)> {
    if !filesystem.inode_is_allocated_checked(device, inode_number)? {
        return Err(Ext4Error::not_found().with_operation("xattr:unallocated_inode"));
    }
    filesystem.get_inode_record(device, inode_number)
}

fn find_value(
    entries: Vec<StoredXattr>,
    namespace: XattrNamespace,
    name: &[u8],
) -> Ext4Result<Option<Vec<u8>>> {
    let Some(entry) = entries
        .into_iter()
        .find(|entry| entry.name.namespace == namespace && entry.name.name == name)
    else {
        return Ok(None);
    };
    match entry.value {
        StoredXattrValue::Local(value) => Ok(Some(value)),
        StoredXattrValue::EaInode { .. } => Err(Ext4Error::unsupported_feature(
            crate::error::FeatureSet::Incompatible,
            Ext4Superblock::EXT4_FEATURE_INCOMPAT_EA_INODE,
        )
        .with_operation("xattr:ea_inode_value")),
    }
}

fn validate_name(name: &[u8]) -> Ext4Result<()> {
    if name.len() > u8::MAX as usize || name.contains(&0) {
        return Err(Ext4Error::invalid_input().with_operation("xattr:name"));
    }
    Ok(())
}

pub(super) fn has_valid_inline_store(
    filesystem: &Ext4FileSystem,
    inode: &Ext4Inode,
    raw_inode: &[u8],
) -> Ext4Result<bool> {
    parse_inline_store(filesystem, inode, raw_inode).map(|entries| entries.is_some())
}

fn parse_inline_store(
    filesystem: &Ext4FileSystem,
    inode: &Ext4Inode,
    raw_inode: &[u8],
) -> Ext4Result<Option<Vec<StoredXattr>>> {
    let xattr_offset = usize::from(Ext4Inode::GOOD_OLD_INODE_SIZE)
        .checked_add(usize::from(inode.i_extra_isize))
        .ok_or_else(Ext4Error::overflow)?;
    let minimum_end = xattr_offset
        .checked_add(XATTR_IBODY_HEADER_SIZE + XATTR_TERMINATOR_SIZE)
        .ok_or_else(Ext4Error::overflow)?;
    if minimum_end > raw_inode.len() {
        return Ok(None);
    }
    let xattrs = &raw_inode[xattr_offset..];
    if read_u32_le(&xattrs[..XATTR_IBODY_HEADER_SIZE]) != XATTR_MAGIC {
        return Ok(None);
    }
    parse_xattrs(
        &filesystem.superblock,
        filesystem.root_inode,
        xattrs,
        XATTR_IBODY_HEADER_SIZE,
        XATTR_IBODY_HEADER_SIZE,
    )
    .map(Some)
}

pub(super) fn read_external_store<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    filesystem: &Ext4FileSystem,
    inode: &Ext4Inode,
) -> Ext4Result<Option<ExternalStore>> {
    let block_number = inode.file_acl();
    if block_number == 0 {
        return Ok(None);
    }
    if block_number >= filesystem.superblock.blocks_count() || block_number >= device.total_blocks()
    {
        return Err(Ext4Error::corrupted().with_operation("xattr:block_number"));
    }
    let block_number = AbsoluteBN::new(block_number);
    let mut block = alloc::vec![0u8; filesystem.block_size()];
    device.read_blocks(&mut block, block_number, 1)?;
    validate_external_header(&filesystem.superblock, block_number, &block)?;
    let entries = parse_xattrs(
        &filesystem.superblock,
        filesystem.root_inode,
        &block,
        XATTR_BLOCK_HEADER_SIZE,
        0,
    )?;
    Ok(Some(ExternalStore {
        block: block_number,
        refcount: read_u32_le(&block[4..8]),
        entries,
        raw: block,
    }))
}

pub(super) fn external_store_revoke_records(external: Option<&ExternalStore>) -> usize {
    usize::from(external.is_some_and(|external| external.refcount == 1))
}

/// Drops one inode's reference to a checked external xattr store.
///
/// The caller must run this transition in the same metadata transaction that
/// clears the inode's `i_file_acl`. A shared store remains allocated with its
/// checksum updated; an exclusively owned store is revoked before allocator
/// reuse and returns the block group whose bitmap metadata changed.
pub(super) fn release_external_store<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    filesystem: &mut Ext4FileSystem,
    external: ExternalStore,
) -> Ext4Result<Option<BGIndex>> {
    if external.refcount > 1 {
        let mut image = external.raw;
        image[4..8].copy_from_slice(&(external.refcount - 1).to_le_bytes());
        set_external_block_checksum(&filesystem.superblock, external.block, &mut image)?;
        device.write_blocks(&image, external.block, 1, true)?;
        return Ok(None);
    }

    device.forget_detached_metadata(external.block)?;
    filesystem.free_block(device, external.block)?;
    let (group, _) = filesystem.block_allocator.global_to_group(external.block)?;
    Ok(Some(group))
}

fn validate_external_header(
    superblock: &Ext4Superblock,
    block_number: AbsoluteBN,
    block: &[u8],
) -> Ext4Result<()> {
    if block.len() < XATTR_BLOCK_HEADER_SIZE + XATTR_TERMINATOR_SIZE
        || read_u32_le(&block[0..4]) != XATTR_MAGIC
        || read_u32_le(&block[8..12]) != 1
    {
        return Err(Ext4Error::corrupted().with_operation("xattr:block_header"));
    }
    let refcount = read_u32_le(&block[4..8]);
    if refcount == 0 || refcount > XATTR_REFCOUNT_MAX {
        return Err(Ext4Error::corrupted().with_operation("xattr:block_refcount"));
    }
    if ext4_superblock_has_metadata_csum(superblock) {
        let stored =
            read_u32_le(&block[XATTR_BLOCK_CHECKSUM_OFFSET..XATTR_BLOCK_CHECKSUM_OFFSET + 4]);
        let zero = 0u32.to_le_bytes();
        let block_number = block_number.raw().to_le_bytes();
        let expected = ext4_metadata_csum32(
            ext4_crc32c_seed_from_superblock(superblock),
            &[
                &block_number,
                &block[..XATTR_BLOCK_CHECKSUM_OFFSET],
                &zero,
                &block[XATTR_BLOCK_CHECKSUM_OFFSET + 4..],
            ],
        );
        if stored != expected {
            return Err(Ext4Error::checksum().with_operation("xattr:block_checksum"));
        }
    }
    Ok(())
}

fn parse_xattrs(
    superblock: &Ext4Superblock,
    root_inode: InodeNumber,
    storage: &[u8],
    entries_start: usize,
    value_base: usize,
) -> Ext4Result<Vec<StoredXattr>> {
    let mut entry_offsets = Vec::new();
    let mut entry_offset = entries_start;
    loop {
        let prefix_end = entry_offset
            .checked_add(XATTR_TERMINATOR_SIZE)
            .ok_or_else(Ext4Error::overflow)?;
        let prefix = storage
            .get(entry_offset..prefix_end)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("xattr:entry_bounds"))?;
        if read_u32_le(prefix) == 0 {
            break;
        }
        let fixed_end = entry_offset
            .checked_add(XATTR_ENTRY_SIZE)
            .ok_or_else(Ext4Error::overflow)?;
        let fixed = storage
            .get(entry_offset..fixed_end)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("xattr:entry_bounds"))?;
        let name_len = usize::from(fixed[0]);
        let name_end = fixed_end
            .checked_add(name_len)
            .ok_or_else(Ext4Error::overflow)?;
        let name = storage
            .get(fixed_end..name_end)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("xattr:name_bounds"))?;
        let next = entry_offset
            .checked_add(round_up_4(
                XATTR_ENTRY_SIZE
                    .checked_add(name_len)
                    .ok_or_else(Ext4Error::overflow)?,
            )?)
            .ok_or_else(Ext4Error::overflow)?;
        if name.contains(&0)
            || next
                .checked_add(XATTR_TERMINATOR_SIZE)
                .is_none_or(|end| end > storage.len())
        {
            return Err(Ext4Error::corrupted().with_operation("xattr:name"));
        }
        entry_offsets.push(entry_offset);
        entry_offset = next;
    }
    let names_end = entry_offset
        .checked_add(XATTR_TERMINATOR_SIZE)
        .ok_or_else(Ext4Error::overflow)?;

    let mut entries = Vec::with_capacity(entry_offsets.len());
    for entry_offset in entry_offsets {
        let fixed_end = entry_offset + XATTR_ENTRY_SIZE;
        let fixed = &storage[entry_offset..fixed_end];
        let name_len = usize::from(fixed[0]);
        let name = storage[fixed_end..fixed_end + name_len].to_vec();
        let value_offset = usize::from(read_u16_le(&fixed[2..4]));
        let value_inode = read_u32_le(&fixed[4..8]);
        let value_size = read_u32_le(&fixed[8..12]);
        if value_size > XATTR_SIZE_MAX {
            return Err(Ext4Error::corrupted().with_operation("xattr:value_size"));
        }
        let value = if value_inode != 0 {
            if !superblock.has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_EA_INODE)
                || value_inode == root_inode.raw()
                || value_inode < superblock.s_first_ino
                || value_inode > superblock.s_inodes_count
                || value_size == 0
            {
                return Err(Ext4Error::corrupted().with_operation("xattr:value_inode"));
            }
            StoredXattrValue::EaInode {
                inode: value_inode,
                size: value_size,
            }
        } else {
            let value_size = usize::try_from(value_size).map_err(|_| Ext4Error::overflow())?;
            if value_size == 0 {
                StoredXattrValue::Local(Vec::new())
            } else {
                let absolute_offset = value_base
                    .checked_add(value_offset)
                    .ok_or_else(Ext4Error::overflow)?;
                let value_end = absolute_offset
                    .checked_add(value_size)
                    .ok_or_else(Ext4Error::overflow)?;
                let padded_end = absolute_offset
                    .checked_add(round_up_4(value_size)?)
                    .ok_or_else(Ext4Error::overflow)?;
                if value_offset > storage.len().saturating_sub(value_base)
                    || absolute_offset < names_end
                    || value_end > storage.len()
                    || padded_end > storage.len()
                {
                    return Err(Ext4Error::corrupted().with_operation("xattr:value_bounds"));
                }
                StoredXattrValue::Local(storage[absolute_offset..value_end].to_vec())
            }
        };
        entries.push(StoredXattr {
            name: XattrName {
                namespace: XattrNamespace::from_disk_index(fixed[1]),
                name,
            },
            value,
        });
    }
    Ok(entries)
}

fn round_up_4(value: usize) -> Ext4Result<usize> {
    value
        .checked_add(3)
        .map(|value| value & !3)
        .ok_or_else(Ext4Error::overflow)
}

#[cfg(test)]
mod fault_tests {
    use alloc::{rc::Rc, vec, vec::Vec};
    use core::cell::Cell;

    use super::*;
    use crate::{
        BLOCK_SIZE, DeviceCapabilities, DeviceGeometry, Ext4Timestamp, SectorId, dir, mkfile, mkfs,
    };

    struct FailingMemoryDevice {
        bytes: Vec<u8>,
        fail_write_sector: Rc<Cell<Option<u64>>>,
    }

    impl FailingMemoryDevice {
        fn new(blocks: usize) -> (Self, Rc<Cell<Option<u64>>>) {
            let fail_write_sector = Rc::new(Cell::new(None));
            (
                Self {
                    bytes: vec![0; blocks * BLOCK_SIZE],
                    fail_write_sector: Rc::clone(&fail_write_sector),
                },
                fail_write_sector,
            )
        }
    }

    impl BlockIo for FailingMemoryDevice {
        fn write(&mut self, buffer: &[u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
            if self.fail_write_sector.get() == Some(sector.raw()) {
                self.fail_write_sector.set(None);
                return Err(Ext4Error::io());
            }
            let start = sector.as_usize()? * BLOCK_SIZE;
            let end = start
                .checked_add(buffer.len())
                .ok_or_else(Ext4Error::overflow)?;
            self.bytes
                .get_mut(start..end)
                .ok_or_else(Ext4Error::io)?
                .copy_from_slice(buffer);
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
            let start = sector.as_usize()? * BLOCK_SIZE;
            let end = start
                .checked_add(buffer.len())
                .ok_or_else(Ext4Error::overflow)?;
            buffer.copy_from_slice(self.bytes.get(start..end).ok_or_else(Ext4Error::io)?);
            Ok(())
        }

        fn geometry(&self) -> DeviceGeometry {
            DeviceGeometry::new(BLOCK_SIZE as u32, (self.bytes.len() / BLOCK_SIZE) as u64)
        }

        fn capabilities(&self) -> DeviceCapabilities {
            DeviceCapabilities {
                flush: true,
                ..DeviceCapabilities::default()
            }
        }

        fn flush(&mut self) -> Ext4Result<()> {
            Ok(())
        }
    }

    impl Clock for FailingMemoryDevice {
        fn now(&self) -> Ext4Result<Ext4Timestamp> {
            Ok(Ext4Timestamp::new(1_700_000_000, 0))
        }
    }

    fn create_shared_external_xattr(
        journal: &mut Jbd2Dev<FailingMemoryDevice>,
        filesystem: &mut Ext4FileSystem,
    ) -> (InodeNumber, InodeNumber, AbsoluteBN) {
        mkfile(journal, filesystem, "/first", None, None).expect("create first file");
        mkfile(journal, filesystem, "/second", None, None).expect("create second file");
        filesystem
            .sync_filesystem(journal)
            .expect("sync baseline files");
        journal
            .umount_commit()
            .expect("commit baseline file metadata");

        let first = dir::get_inode_with_num(filesystem, journal, "/first")
            .expect("lookup first file")
            .expect("first file must exist")
            .0;
        let second = dir::get_inode_with_num(filesystem, journal, "/second")
            .expect("lookup second file")
            .expect("second file must exist")
            .0;
        set_inode_xattr(
            journal,
            filesystem,
            first,
            XattrNamespace::User,
            b"shared",
            &vec![0x5a; 512],
            XattrSetMode::Create,
        )
        .expect("create external xattr");
        filesystem
            .sync_filesystem(journal)
            .expect("sync external xattr");
        journal.umount_commit().expect("commit external xattr");

        let (first_inode, _) = filesystem
            .get_inode_record(journal, first)
            .expect("read first inode");
        let external = read_external_store(journal, filesystem, &first_inode)
            .expect("read external xattr")
            .expect("xattr must be external");
        let shared_block = external.block;
        let block_size = filesystem.block_size() as u32;
        let huge_file = filesystem
            .superblock
            .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE);
        let mut shared_image = external.raw;
        shared_image[4..8].copy_from_slice(&2u32.to_le_bytes());
        set_external_block_checksum(&filesystem.superblock, shared_block, &mut shared_image)
            .expect("checksum shared xattr block");

        filesystem
            .with_metadata_transaction(journal, 2, |filesystem, journal| {
                journal.write_blocks(&shared_image, shared_block, 1, true)?;
                filesystem.modify_inode_record(journal, second, |inode, _| {
                    let sectors = inode
                        .blocks_count(block_size, huge_file)
                        .checked_add(u64::from(block_size / 512))
                        .ok_or_else(Ext4Error::overflow)?;
                    inode.set_blocks_count(sectors, block_size, huge_file)?;
                    inode.set_file_acl(shared_block.raw())?;
                    Ok(())
                })?;
                filesystem.inodetable_cache.flush(journal, second)
            })
            .expect("publish shared xattr reference");
        filesystem
            .sync_filesystem(journal)
            .expect("sync shared xattr fixture");
        journal
            .umount_commit()
            .expect("commit shared xattr fixture");

        assert_eq!(
            get_inode_xattr(journal, filesystem, second, XattrNamespace::User, b"shared",)
                .expect("second inode must share xattr"),
            vec![0x5a; 512]
        );
        (first, second, shared_block)
    }

    #[test]
    fn inode_write_failure_does_not_publish_inline_xattr_in_cache() {
        let (device, fail_write_sector) = FailingMemoryDevice::new(32 * 1024);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut journal).expect("mkfs must succeed");
        let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount must succeed");
        mkfile(&mut journal, &mut filesystem, "/victim", None, None).expect("create baseline file");
        filesystem
            .sync_filesystem(&mut journal)
            .expect("sync baseline filesystem");
        journal.umount_commit().expect("commit baseline metadata");
        journal
            .set_journal_use(false)
            .expect("switch test to direct metadata writes");

        let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/victim")
            .expect("lookup victim")
            .expect("victim must exist")
            .0;
        let (_, before_raw) = filesystem
            .get_inode_record(&mut journal, inode_number)
            .expect("read baseline inode");
        let (group, _) = filesystem
            .inode_allocator
            .global_to_group(inode_number)
            .expect("locate inode group");
        let inode_table = filesystem
            .get_group_desc(group)
            .expect("inode group descriptor")
            .inode_table();
        let (inode_block, ..) = filesystem
            .inodetable_cache
            .calc_inode_location(
                inode_number,
                filesystem.superblock.s_inodes_per_group,
                AbsoluteBN::new(inode_table),
                filesystem.block_size(),
            )
            .expect("locate inode table block");
        fail_write_sector.set(Some(inode_block.raw()));

        let error = set_inode_xattr(
            &mut journal,
            &mut filesystem,
            inode_number,
            XattrNamespace::User,
            b"atomic",
            b"old-or-new-never-half",
            XattrSetMode::Create,
        )
        .expect_err("injected inode-table write must fail");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        let (_, after_raw) = filesystem
            .get_inode_record(&mut journal, inode_number)
            .expect("read inode after rollback");
        assert_eq!(
            after_raw, before_raw,
            "failed xattr leaked into inode cache"
        );
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut filesystem,
                inode_number,
                XattrNamespace::User,
                b"atomic",
            )
            .expect_err("failed xattr must remain absent")
            .kind(),
            Ext4ErrorKind::NotFound
        );
    }

    #[test]
    fn xattr_placement_keeps_small_value_inline_when_large_value_needs_a_block() {
        let (device, _) = FailingMemoryDevice::new(32 * 1024);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut journal).expect("mkfs must succeed");
        let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount must succeed");
        mkfile(&mut journal, &mut filesystem, "/victim", None, None).expect("create baseline file");
        let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/victim")
            .expect("lookup victim")
            .expect("victim must exist")
            .0;
        let small_value = vec![0x11; 48];
        let large_value = vec![0x22; 4_000];

        set_inode_xattr(
            &mut journal,
            &mut filesystem,
            inode_number,
            XattrNamespace::User,
            b"small",
            &small_value,
            XattrSetMode::Create,
        )
        .expect("store small xattr inline");
        set_inode_xattr(
            &mut journal,
            &mut filesystem,
            inode_number,
            XattrNamespace::User,
            b"large",
            &large_value,
            XattrSetMode::Create,
        )
        .expect("store large xattr externally without moving the small value");

        let (inode, raw_inode) = filesystem
            .get_inode_record(&mut journal, inode_number)
            .expect("read inode with split xattr stores");
        let inline = parse_inline_store(&filesystem, &inode, &raw_inode)
            .expect("parse inline store")
            .expect("small xattr must remain inline");
        let external = read_external_store(&mut journal, &filesystem, &inode)
            .expect("read external store")
            .expect("large xattr must use an external block");
        assert_eq!(inline.len(), 1);
        assert_eq!(inline[0].name.name, b"small");
        assert_eq!(external.entries.len(), 1);
        assert_eq!(external.entries[0].name.name, b"large");
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut filesystem,
                inode_number,
                XattrNamespace::User,
                b"small",
            )
            .expect("read inline xattr"),
            small_value
        );
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut filesystem,
                inode_number,
                XattrNamespace::User,
                b"large",
            )
            .expect("read external xattr"),
            large_value
        );
    }

    #[test]
    fn failed_split_store_inode_publish_restores_inline_value_and_allocation() {
        let (device, fail_write_sector) = FailingMemoryDevice::new(32 * 1024);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut journal).expect("mkfs must succeed");
        let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount must succeed");
        mkfile(&mut journal, &mut filesystem, "/victim", None, None).expect("create baseline file");
        let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/victim")
            .expect("lookup victim")
            .expect("victim must exist")
            .0;
        let small_value = vec![0x11; 48];
        set_inode_xattr(
            &mut journal,
            &mut filesystem,
            inode_number,
            XattrNamespace::User,
            b"small",
            &small_value,
            XattrSetMode::Create,
        )
        .expect("store baseline inline xattr");
        filesystem
            .sync_filesystem(&mut journal)
            .expect("sync baseline xattr");
        journal.umount_commit().expect("commit baseline xattr");
        let before_free_blocks = filesystem.statfs().free_blocks;
        let (_, before_raw) = filesystem
            .get_inode_record(&mut journal, inode_number)
            .expect("read baseline inode");
        let (group, _) = filesystem
            .inode_allocator
            .global_to_group(inode_number)
            .expect("locate inode group");
        let inode_table = filesystem
            .get_group_desc(group)
            .expect("inode group descriptor")
            .inode_table();
        let (inode_block, ..) = filesystem
            .inodetable_cache
            .calc_inode_location(
                inode_number,
                filesystem.superblock.s_inodes_per_group,
                AbsoluteBN::new(inode_table),
                filesystem.block_size(),
            )
            .expect("locate inode table block");
        journal
            .set_journal_use(false)
            .expect("switch test to direct metadata writes");
        fail_write_sector.set(Some(inode_block.raw()));

        let error = set_inode_xattr(
            &mut journal,
            &mut filesystem,
            inode_number,
            XattrNamespace::User,
            b"large",
            &vec![0x22; 4_000],
            XattrSetMode::Create,
        )
        .expect_err("inode-table failure must abort split-store publication");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);
        assert_eq!(filesystem.statfs().free_blocks, before_free_blocks);
        let (restored_inode, restored_raw) = filesystem
            .get_inode_record(&mut journal, inode_number)
            .expect("read restored inode");
        assert_eq!(restored_raw, before_raw);
        assert_eq!(restored_inode.file_acl(), 0);
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut filesystem,
                inode_number,
                XattrNamespace::User,
                b"small",
            )
            .expect("read restored inline xattr"),
            small_value
        );
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut filesystem,
                inode_number,
                XattrNamespace::User,
                b"large",
            )
            .expect_err("failed external xattr must remain absent")
            .kind(),
            Ext4ErrorKind::NotFound
        );

        drop(filesystem);
        let device = journal.into_inner();
        let mut remount_journal = Jbd2Dev::initial_jbd2dev(0, device, false);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_journal).expect("remount rolled-back split store");
        assert_eq!(remounted.statfs().free_blocks, before_free_blocks);
        let inode = remounted
            .get_inode_by_num(&mut remount_journal, inode_number)
            .expect("read remounted inode");
        assert_eq!(inode.file_acl(), 0);
        assert_eq!(
            get_inode_xattr(
                &mut remount_journal,
                &mut remounted,
                inode_number,
                XattrNamespace::User,
                b"small",
            )
            .expect("read remounted inline xattr"),
            vec![0x11; 48]
        );
        assert_eq!(
            get_inode_xattr(
                &mut remount_journal,
                &mut remounted,
                inode_number,
                XattrNamespace::User,
                b"large",
            )
            .expect_err("failed external xattr remains absent after remount")
            .kind(),
            Ext4ErrorKind::NotFound
        );
    }

    #[test]
    fn journal_credit_failure_restores_external_xattr_allocation() {
        let (device, _) = FailingMemoryDevice::new(32 * 1024);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut journal).expect("mkfs must succeed");
        let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount must succeed");
        mkfile(&mut journal, &mut filesystem, "/victim", None, None).expect("create baseline file");
        filesystem
            .sync_filesystem(&mut journal)
            .expect("sync baseline filesystem");
        journal.umount_commit().expect("commit baseline metadata");

        let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/victim")
            .expect("lookup victim")
            .expect("victim must exist")
            .0;
        let (inode, before_raw) = filesystem
            .get_inode_record(&mut journal, inode_number)
            .expect("read baseline inode");
        let before_free_blocks = filesystem.statfs().free_blocks;
        let entries = vec![StoredXattr {
            name: XattrName {
                namespace: XattrNamespace::User,
                name: b"external".to_vec(),
            },
            value: StoredXattrValue::Local(vec![0x5a; 512]),
        }];

        let error = persist_xattrs(
            &mut journal,
            &mut filesystem,
            XattrPersistence {
                inode_number,
                original_inode: &inode,
                layout: XattrLayout {
                    inline: Vec::new(),
                    external: entries,
                },
                external_store: None,
                transaction_credits: 2,
            },
        )
        .expect_err("two credits cannot publish xattr, inode, bitmap, GDT, and superblock");
        assert_eq!(error.kind(), Ext4ErrorKind::NoSpace);
        assert_eq!(filesystem.statfs().free_blocks, before_free_blocks);
        let (_, after_raw) = filesystem
            .get_inode_record(&mut journal, inode_number)
            .expect("read inode after journal rollback");
        assert_eq!(after_raw, before_raw);
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut filesystem,
                inode_number,
                XattrNamespace::User,
                b"external",
            )
            .expect_err("aborted external xattr must remain absent")
            .kind(),
            Ext4ErrorKind::NotFound
        );

        filesystem
            .umount(&mut journal)
            .expect("unmount rolled-back filesystem");
        let mut remounted =
            Ext4FileSystem::mount(&mut journal).expect("remount rolled-back filesystem");
        assert_eq!(remounted.statfs().free_blocks, before_free_blocks);
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut remounted,
                inode_number,
                XattrNamespace::User,
                b"external",
            )
            .expect_err("external xattr must remain absent after remount")
            .kind(),
            Ext4ErrorKind::NotFound
        );
    }

    #[test]
    fn reaping_inode_releases_its_external_xattr_block() {
        let (device, _) = FailingMemoryDevice::new(32 * 1024);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut journal).expect("mkfs must succeed");
        let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount must succeed");
        mkfile(&mut journal, &mut filesystem, "/victim", None, None).expect("create baseline file");
        filesystem
            .sync_filesystem(&mut journal)
            .expect("sync baseline filesystem");
        journal.umount_commit().expect("commit baseline metadata");

        let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/victim")
            .expect("lookup victim")
            .expect("victim must exist")
            .0;
        let free_blocks_before_xattr = filesystem.statfs().free_blocks;
        set_inode_xattr(
            &mut journal,
            &mut filesystem,
            inode_number,
            XattrNamespace::User,
            b"reap",
            &vec![0x5a; 512],
            XattrSetMode::Create,
        )
        .expect("create external xattr");
        filesystem
            .sync_filesystem(&mut journal)
            .expect("sync external xattr");
        journal.umount_commit().expect("commit external xattr");

        let inode = filesystem
            .get_inode_by_num(&mut journal, inode_number)
            .expect("read inode with external xattr");
        assert_ne!(inode.file_acl(), 0, "fixture must use an external block");
        assert_eq!(
            filesystem.statfs().free_blocks + 1,
            free_blocks_before_xattr
        );

        let outcome =
            crate::file::unlink(&mut filesystem, &mut journal, "/victim").expect("unlink victim");
        assert!(outcome.requires_reap());
        crate::file::reap_unlinked_inode(&mut filesystem, &mut journal, outcome.inode)
            .expect("reap victim");
        assert_eq!(filesystem.statfs().free_blocks, free_blocks_before_xattr);

        filesystem
            .umount(&mut journal)
            .expect("unmount reaped filesystem");
        let remounted = Ext4FileSystem::mount(&mut journal).expect("remount reaped filesystem");
        assert_eq!(remounted.statfs().free_blocks, free_blocks_before_xattr);
    }

    #[test]
    fn reaping_shared_external_xattr_releases_only_the_final_reference() {
        let (device, _) = FailingMemoryDevice::new(32 * 1024);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut journal).expect("mkfs must succeed");
        let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount must succeed");
        let (first, second, shared_block) =
            create_shared_external_xattr(&mut journal, &mut filesystem);
        let free_blocks_with_shared_xattr = filesystem.statfs().free_blocks;

        let first_outcome = crate::file::unlink(&mut filesystem, &mut journal, "/first")
            .expect("unlink first inode");
        assert_eq!(first_outcome.inode, first);
        crate::file::reap_unlinked_inode(&mut filesystem, &mut journal, first)
            .expect("reap first shared reference");
        assert_eq!(
            filesystem.statfs().free_blocks,
            free_blocks_with_shared_xattr,
            "the shared external block must remain allocated"
        );
        let second_inode = filesystem
            .get_inode_by_num(&mut journal, second)
            .expect("read surviving inode");
        let surviving_store = read_external_store(&mut journal, &filesystem, &second_inode)
            .expect("read surviving external store")
            .expect("surviving inode keeps external xattr");
        assert_eq!(surviving_store.block, shared_block);
        assert_eq!(surviving_store.refcount, 1);
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut filesystem,
                second,
                XattrNamespace::User,
                b"shared",
            )
            .expect("read surviving shared xattr"),
            vec![0x5a; 512]
        );

        filesystem
            .umount(&mut journal)
            .expect("unmount after first shared reap");
        let mut remounted =
            Ext4FileSystem::mount(&mut journal).expect("remount after first shared reap");
        let persisted_inode = remounted
            .get_inode_by_num(&mut journal, second)
            .expect("read persisted surviving inode");
        let persisted_store = read_external_store(&mut journal, &remounted, &persisted_inode)
            .expect("read persisted external store")
            .expect("persisted external store must remain");
        assert_eq!(persisted_store.refcount, 1);

        let second_outcome = crate::file::unlink(&mut remounted, &mut journal, "/second")
            .expect("unlink final inode");
        assert_eq!(second_outcome.inode, second);
        crate::file::reap_unlinked_inode(&mut remounted, &mut journal, second)
            .expect("reap final shared reference");
        assert_eq!(
            remounted.statfs().free_blocks,
            free_blocks_with_shared_xattr + 1
        );
    }

    #[test]
    fn failed_external_xattr_reap_restores_orphan_and_allocation() {
        let (device, fail_write_sector) = FailingMemoryDevice::new(32 * 1024);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut journal).expect("mkfs must succeed");
        let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount must succeed");
        mkfile(&mut journal, &mut filesystem, "/victim", None, None).expect("create baseline file");
        filesystem
            .sync_filesystem(&mut journal)
            .expect("sync baseline filesystem");
        journal.umount_commit().expect("commit baseline metadata");

        let inode_number = dir::get_inode_with_num(&mut filesystem, &mut journal, "/victim")
            .expect("lookup victim")
            .expect("victim must exist")
            .0;
        let free_blocks_before_xattr = filesystem.statfs().free_blocks;
        set_inode_xattr(
            &mut journal,
            &mut filesystem,
            inode_number,
            XattrNamespace::User,
            b"reap",
            &vec![0x5a; 512],
            XattrSetMode::Create,
        )
        .expect("create external xattr");
        filesystem
            .sync_filesystem(&mut journal)
            .expect("sync external xattr");
        journal.umount_commit().expect("commit external xattr");
        let inode = filesystem
            .get_inode_by_num(&mut journal, inode_number)
            .expect("read inode with external xattr");
        let external = read_external_store(&mut journal, &filesystem, &inode)
            .expect("read external store")
            .expect("fixture must use external xattr");
        let (xattr_group, _) = filesystem
            .block_allocator
            .global_to_group(external.block)
            .expect("locate external xattr group");
        let block_bitmap = filesystem
            .get_group_desc(xattr_group)
            .expect("read external xattr group descriptor")
            .block_bitmap();

        journal
            .set_journal_use(false)
            .expect("switch fixture to direct metadata writes");
        let outcome =
            crate::file::unlink(&mut filesystem, &mut journal, "/victim").expect("unlink victim");
        assert!(outcome.requires_reap());
        let free_blocks_before_reap = filesystem.statfs().free_blocks;
        fail_write_sector.set(Some(block_bitmap));

        let error = crate::file::reap_unlinked_inode(&mut filesystem, &mut journal, outcome.inode)
            .expect_err("external xattr bitmap publication must fail");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);
        assert_eq!(filesystem.statfs().free_blocks, free_blocks_before_reap);
        assert!(
            filesystem
                .inode_is_allocated_checked(&mut journal, inode_number)
                .expect("check restored inode allocation")
        );
        assert!(
            filesystem
                .orphan_contains(&mut journal, inode_number)
                .expect("check restored orphan membership")
        );
        let restored_inode = filesystem
            .get_inode_by_num(&mut journal, inode_number)
            .expect("read restored inode");
        assert_eq!(restored_inode.file_acl(), external.block.raw());
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut filesystem,
                inode_number,
                XattrNamespace::User,
                b"reap",
            )
            .expect("read restored xattr"),
            vec![0x5a; 512]
        );

        crate::file::reap_unlinked_inode(&mut filesystem, &mut journal, inode_number)
            .expect("retry external xattr reap");
        assert_eq!(filesystem.statfs().free_blocks, free_blocks_before_xattr);
    }

    #[test]
    fn shared_external_xattr_cow_preserves_the_other_inode() {
        let (device, _) = FailingMemoryDevice::new(32 * 1024);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut journal).expect("mkfs must succeed");
        let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount must succeed");
        let (first, second, shared_block) =
            create_shared_external_xattr(&mut journal, &mut filesystem);
        let before_free_blocks = filesystem.statfs().free_blocks;

        set_inode_xattr(
            &mut journal,
            &mut filesystem,
            first,
            XattrNamespace::User,
            b"shared",
            &vec![0x33; 512],
            XattrSetMode::Replace,
        )
        .expect("copy shared xattr block");

        let first_inode = filesystem
            .get_inode_by_num(&mut journal, first)
            .expect("read first inode after COW");
        let first_store = read_external_store(&mut journal, &filesystem, &first_inode)
            .expect("read first external store")
            .expect("first xattr remains external");
        let second_inode = filesystem
            .get_inode_by_num(&mut journal, second)
            .expect("read second inode after COW");
        let second_store = read_external_store(&mut journal, &filesystem, &second_inode)
            .expect("read second external store")
            .expect("second xattr remains external");
        assert_ne!(first_store.block, shared_block);
        assert_eq!(first_store.refcount, 1);
        assert_eq!(second_store.block, shared_block);
        assert_eq!(second_store.refcount, 1);
        assert_eq!(filesystem.statfs().free_blocks + 1, before_free_blocks);
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut filesystem,
                first,
                XattrNamespace::User,
                b"shared",
            )
            .expect("read copied xattr"),
            vec![0x33; 512]
        );
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut filesystem,
                second,
                XattrNamespace::User,
                b"shared",
            )
            .expect("read retained shared xattr"),
            vec![0x5a; 512]
        );

        filesystem
            .umount(&mut journal)
            .expect("unmount copied xattr filesystem");
        let mut remounted =
            Ext4FileSystem::mount(&mut journal).expect("remount copied xattr filesystem");
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut remounted,
                first,
                XattrNamespace::User,
                b"shared",
            )
            .expect("read copied xattr after remount"),
            vec![0x33; 512]
        );
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut remounted,
                second,
                XattrNamespace::User,
                b"shared",
            )
            .expect("read retained xattr after remount"),
            vec![0x5a; 512]
        );
    }

    #[test]
    fn unrelated_inline_update_does_not_copy_or_rewrite_shared_external_store() {
        let (device, fail_write_sector) = FailingMemoryDevice::new(32 * 1024);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut journal).expect("mkfs must succeed");
        let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount must succeed");
        let (first, second, shared_block) =
            create_shared_external_xattr(&mut journal, &mut filesystem);
        let before_free_blocks = filesystem.statfs().free_blocks;
        journal
            .set_journal_use(false)
            .expect("switch test to direct metadata writes");
        fail_write_sector.set(Some(shared_block.raw()));

        set_inode_xattr(
            &mut journal,
            &mut filesystem,
            first,
            XattrNamespace::User,
            b"inline",
            b"unrelated",
            XattrSetMode::Create,
        )
        .expect("an inline update must not rewrite the unchanged shared block");

        assert_eq!(
            fail_write_sector.get(),
            Some(shared_block.raw()),
            "the unchanged shared block must not receive a write"
        );
        assert_eq!(filesystem.statfs().free_blocks, before_free_blocks);
        for inode_number in [first, second] {
            let inode = filesystem
                .get_inode_by_num(&mut journal, inode_number)
                .expect("read inode retaining shared store");
            let store = read_external_store(&mut journal, &filesystem, &inode)
                .expect("read shared store")
                .expect("shared store remains present");
            assert_eq!(store.block, shared_block);
            assert_eq!(store.refcount, 2);
            assert_eq!(
                get_inode_xattr(
                    &mut journal,
                    &mut filesystem,
                    inode_number,
                    XattrNamespace::User,
                    b"shared",
                )
                .expect("read unchanged shared xattr"),
                vec![0x5a; 512]
            );
        }
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut filesystem,
                first,
                XattrNamespace::User,
                b"inline",
            )
            .expect("read new inline xattr"),
            b"unrelated"
        );
        assert_eq!(
            get_inode_xattr(
                &mut journal,
                &mut filesystem,
                second,
                XattrNamespace::User,
                b"inline",
            )
            .expect_err("the sibling inode must not gain the inline xattr")
            .kind(),
            Ext4ErrorKind::NotFound
        );
    }

    #[test]
    fn shared_external_xattr_credit_failure_restores_both_references() {
        let (device, _) = FailingMemoryDevice::new(32 * 1024);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut journal).expect("mkfs must succeed");
        let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount must succeed");
        let (first, second, shared_block) =
            create_shared_external_xattr(&mut journal, &mut filesystem);
        let before_free_blocks = filesystem.statfs().free_blocks;
        let (first_inode, first_raw) = filesystem
            .get_inode_record(&mut journal, first)
            .expect("read first inode before failed COW");
        let (_, second_raw) = filesystem
            .get_inode_record(&mut journal, second)
            .expect("read second inode before failed COW");
        let external = read_external_store(&mut journal, &filesystem, &first_inode)
            .expect("read shared store")
            .expect("shared store must exist");
        let entries = vec![StoredXattr {
            name: XattrName {
                namespace: XattrNamespace::User,
                name: b"shared".to_vec(),
            },
            value: StoredXattrValue::Local(vec![0x33; 512]),
        }];

        let error = persist_xattrs(
            &mut journal,
            &mut filesystem,
            XattrPersistence {
                inode_number: first,
                original_inode: &first_inode,
                layout: XattrLayout {
                    inline: Vec::new(),
                    external: entries,
                },
                external_store: Some(external),
                transaction_credits: 2,
            },
        )
        .expect_err("two credits cannot publish a shared-block COW");
        assert_eq!(error.kind(), Ext4ErrorKind::NoSpace);
        assert_eq!(filesystem.statfs().free_blocks, before_free_blocks);
        assert_eq!(
            filesystem
                .get_inode_record(&mut journal, first)
                .expect("read first inode after rollback")
                .1,
            first_raw
        );
        assert_eq!(
            filesystem
                .get_inode_record(&mut journal, second)
                .expect("read second inode after rollback")
                .1,
            second_raw
        );
        let restored_inode = filesystem
            .get_inode_by_num(&mut journal, first)
            .expect("read restored first inode");
        let restored_store = read_external_store(&mut journal, &filesystem, &restored_inode)
            .expect("read restored shared store")
            .expect("shared store remains present");
        assert_eq!(restored_store.block, shared_block);
        assert_eq!(restored_store.refcount, 2);

        filesystem
            .umount(&mut journal)
            .expect("unmount rolled-back shared xattr filesystem");
        let mut remounted =
            Ext4FileSystem::mount(&mut journal).expect("remount rolled-back filesystem");
        for inode_number in [first, second] {
            assert_eq!(
                get_inode_xattr(
                    &mut journal,
                    &mut remounted,
                    inode_number,
                    XattrNamespace::User,
                    b"shared",
                )
                .expect("shared xattr survives failed COW"),
                vec![0x5a; 512]
            );
        }
        assert_eq!(remounted.statfs().free_blocks, before_free_blocks);
    }

    #[test]
    fn shared_external_xattr_direct_write_failure_restores_disk_state() {
        let (device, fail_write_sector) = FailingMemoryDevice::new(32 * 1024);
        let mut journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        mkfs(&mut journal).expect("mkfs must succeed");
        let mut filesystem = Ext4FileSystem::mount(&mut journal).expect("mount must succeed");
        let (first, second, shared_block) =
            create_shared_external_xattr(&mut journal, &mut filesystem);
        let before_free_blocks = filesystem.statfs().free_blocks;
        journal
            .set_journal_use(false)
            .expect("switch test to direct metadata writes");
        fail_write_sector.set(Some(shared_block.raw()));

        let error = set_inode_xattr(
            &mut journal,
            &mut filesystem,
            first,
            XattrNamespace::User,
            b"shared",
            &vec![0x33; 512],
            XattrSetMode::Replace,
        )
        .expect_err("old shared-block refcount write must fail");
        assert_eq!(error.kind(), Ext4ErrorKind::Io);

        drop(filesystem);
        let device = journal.into_inner();
        let mut remount_journal = Jbd2Dev::initial_jbd2dev(0, device, true);
        let mut remounted =
            Ext4FileSystem::mount(&mut remount_journal).expect("remount after failed direct COW");
        assert_eq!(remounted.statfs().free_blocks, before_free_blocks);
        for inode_number in [first, second] {
            let inode = remounted
                .get_inode_by_num(&mut remount_journal, inode_number)
                .expect("read inode after failed direct COW");
            assert_eq!(inode.file_acl(), shared_block.raw());
            assert_eq!(
                get_inode_xattr(
                    &mut remount_journal,
                    &mut remounted,
                    inode_number,
                    XattrNamespace::User,
                    b"shared",
                )
                .expect("old shared value must survive direct COW failure"),
                vec![0x5a; 512]
            );
        }
        let first_inode = remounted
            .get_inode_by_num(&mut remount_journal, first)
            .expect("read first restored inode");
        let restored_store = read_external_store(&mut remount_journal, &remounted, &first_inode)
            .expect("read restored shared store")
            .expect("shared store must remain present");
        assert_eq!(restored_store.refcount, 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline_xattr_fixture() -> Vec<u8> {
        let mut bytes = alloc::vec![0u8; 64];
        bytes[..4].copy_from_slice(&XATTR_MAGIC.to_le_bytes());
        bytes[4] = 3;
        bytes[5] = XattrNamespace::User.disk_index();
        bytes[6..8].copy_from_slice(&28u16.to_le_bytes());
        bytes[12..16].copy_from_slice(&4u32.to_le_bytes());
        bytes[20..23].copy_from_slice(b"key");
        bytes[32..36].copy_from_slice(b"data");
        bytes
    }

    fn xattr_superblock() -> Ext4Superblock {
        Ext4Superblock {
            s_first_ino: 11,
            s_inodes_count: 128,
            ..Ext4Superblock::default()
        }
    }

    #[test]
    fn checked_inline_xattr_decodes_linux_value_base() {
        let entries = parse_xattrs(
            &xattr_superblock(),
            InodeNumber::new(2).expect("root inode"),
            &inline_xattr_fixture(),
            XATTR_IBODY_HEADER_SIZE,
            XATTR_IBODY_HEADER_SIZE,
        )
        .expect("valid inline xattr");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name.name, b"key");
        assert_eq!(entries[0].value, StoredXattrValue::Local(b"data".to_vec()));
    }

    #[test]
    fn checked_inline_xattr_rejects_value_overlapping_names() {
        let mut bytes = inline_xattr_fixture();
        bytes[6..8].copy_from_slice(&20u16.to_le_bytes());
        let error = parse_xattrs(
            &xattr_superblock(),
            InodeNumber::new(2).expect("root inode"),
            &bytes,
            XATTR_IBODY_HEADER_SIZE,
            XATTR_IBODY_HEADER_SIZE,
        )
        .expect_err("overlapping xattr value must be rejected");
        assert_eq!(error.kind(), crate::error::Ext4ErrorKind::Corrupted);
    }

    #[test]
    fn checked_inline_xattr_rejects_embedded_name_terminator() {
        let mut bytes = inline_xattr_fixture();
        bytes[21] = 0;
        let error = parse_xattrs(
            &xattr_superblock(),
            InodeNumber::new(2).expect("root inode"),
            &bytes,
            XATTR_IBODY_HEADER_SIZE,
            XATTR_IBODY_HEADER_SIZE,
        )
        .expect_err("embedded xattr name terminator must be rejected");
        assert_eq!(error.kind(), crate::error::Ext4ErrorKind::Corrupted);
    }

    #[test]
    fn checked_external_xattr_rejects_invalid_refcounts() {
        let mut block = alloc::vec![0u8; XATTR_BLOCK_HEADER_SIZE + XATTR_TERMINATOR_SIZE];
        block[0..4].copy_from_slice(&XATTR_MAGIC.to_le_bytes());
        block[8..12].copy_from_slice(&1u32.to_le_bytes());

        for invalid_refcount in [0, XATTR_REFCOUNT_MAX + 1] {
            block[4..8].copy_from_slice(&invalid_refcount.to_le_bytes());
            let error =
                validate_external_header(&Ext4Superblock::default(), AbsoluteBN::new(1), &block)
                    .expect_err("invalid xattr refcount must be rejected");
            assert_eq!(error.kind(), crate::error::Ext4ErrorKind::Corrupted);
        }
    }
}

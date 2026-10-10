//! Host initramfs archives, unpacked before the global root is published.

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::{str, time::Duration};

use axfs_ng_vfs::{
    DeviceId, Filesystem, MetadataUpdate, Mountpoint, MutationCredentials, NodePermission,
    NodeType, VfsError,
};
use miniz_oxide::{
    DataFormat, MZFlush, MZStatus,
    inflate::stream::{InflateState, inflate},
};

use crate::{MemoryFs, file::OpenOptions, highlevel::FsContext};

const CPIO_HEADER_LEN: usize = 110;
const MAX_NAME: usize = 4096;
const MAX_INFLATED_SIZE: usize = 1024 * 1024 * 1024;

#[derive(Debug)]
pub enum InitramfsError {
    Corrupt(&'static str),
    UnsupportedCompression,
    InvalidPath,
    InvalidText,
    TooLarge,
    Filesystem(VfsError),
}

impl From<VfsError> for InitramfsError {
    fn from(error: VfsError) -> Self {
        Self::Filesystem(error)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UnpackReport {
    pub archives: usize,
    pub entries: usize,
}

struct Entry<'a> {
    name: &'a str,
    data: &'a [u8],
    ino: u32,
    mode: u32,
    uid: u32,
    gid: u32,
    nlink: u32,
    mtime: u32,
    dev_major: u32,
    dev_minor: u32,
    rdev_major: u32,
    rdev_minor: u32,
}

/// Applies each archive to one unpublished memory root. Later entries may
/// replace entries from earlier archives, like Linux's built-in + external pair.
pub fn unpack_sources(sources: &[&[u8]]) -> Result<(Filesystem, UnpackReport), InitramfsError> {
    let fs = MemoryFs::new_ramfs();
    let root = Mountpoint::new_root(&fs).root_location();
    let context = FsContext::new(root);
    let mut report = UnpackReport::default();
    for source in sources.iter().copied().filter(|source| !source.is_empty()) {
        unpack_stream(source, &context, &mut report, false)?;
    }
    Ok((fs, report))
}

pub fn unpack(archive: &[u8]) -> Result<(Filesystem, UnpackReport), InitramfsError> {
    if archive.is_empty() {
        return Err(InitramfsError::Corrupt("empty external archive"));
    }
    unpack_sources(&[archive])
}

fn unpack_stream(
    input: &[u8],
    context: &FsContext,
    report: &mut UnpackReport,
    compressed: bool,
) -> Result<(), InitramfsError> {
    let mut position = 0;
    let initial_archives = report.archives;
    while position < input.len() {
        while input.get(position) == Some(&0) {
            position += 1;
        }
        if position == input.len() {
            break;
        }
        if input[position..].starts_with(b"070701") || input[position..].starts_with(b"070702") {
            if position % 4 != 0 {
                return Err(InitramfsError::Corrupt("broken cpio padding"));
            }
            let mut links = BTreeMap::new();
            let (next, entries) = parse_newc(&input[position..], |entry| {
                apply_entry(context, entry, &mut links)
            })?;
            position += next;
            report.archives += 1;
            report.entries += entries;
        } else if input[position..].starts_with(&[0x1f, 0x8b]) {
            if compressed {
                return Err(InitramfsError::Corrupt("nested gzip archive"));
            }
            let (inflated, consumed) = inflate_gzip(&input[position..])?;
            unpack_stream(&inflated, context, report, true)?;
            position += consumed;
        } else {
            return Err(InitramfsError::UnsupportedCompression);
        }
    }
    if report.archives == initial_archives {
        return Err(InitramfsError::Corrupt("no cpio archive"));
    }
    Ok(())
}

fn parse_newc<'a>(
    input: &'a [u8],
    mut consume: impl FnMut(Entry<'a>) -> Result<(), InitramfsError>,
) -> Result<(usize, usize), InitramfsError> {
    let mut pos: usize = 0;
    let mut entries = 0;
    loop {
        if pos == input.len()
            || input.get(pos) == Some(&0)
            || input[pos..].starts_with(&[0x1f, 0x8b])
        {
            return Ok((pos, entries));
        }
        let hdr = input
            .get(
                pos..pos
                    .checked_add(CPIO_HEADER_LEN)
                    .ok_or(InitramfsError::TooLarge)?,
            )
            .ok_or(InitramfsError::Corrupt("truncated cpio header"))?;
        let checksum = match &hdr[..6] {
            b"070701" => false,
            b"070702" => true,
            _ => return Err(InitramfsError::Corrupt("invalid cpio magic")),
        };
        let mut fields = [0u32; 13];
        for (index, field) in fields.iter_mut().enumerate() {
            *field = u32::from_str_radix(
                str::from_utf8(&hdr[6 + index * 8..14 + index * 8])
                    .map_err(|_| InitramfsError::Corrupt("non-hex cpio field"))?,
                16,
            )
            .map_err(|_| InitramfsError::Corrupt("non-hex cpio field"))?;
        }
        let name_len = usize::try_from(fields[11]).map_err(|_| InitramfsError::TooLarge)?;
        if !(1..=MAX_NAME).contains(&name_len) {
            return Err(InitramfsError::Corrupt("invalid cpio name length"));
        }
        let name_start = pos
            .checked_add(CPIO_HEADER_LEN)
            .ok_or(InitramfsError::TooLarge)?;
        let name_end = name_start
            .checked_add(name_len)
            .ok_or(InitramfsError::TooLarge)?;
        let name = input
            .get(name_start..name_end)
            .ok_or(InitramfsError::Corrupt("truncated cpio filename"))?;
        if name.last() != Some(&0) || name[..name_len - 1].contains(&0) {
            return Err(InitramfsError::Corrupt("invalid cpio filename terminator"));
        }
        let name =
            str::from_utf8(&name[..name_len - 1]).map_err(|_| InitramfsError::InvalidText)?;
        let data_start = align4(name_end)?;
        if !input
            .get(name_end..data_start)
            .is_some_and(|padding| padding.iter().all(|byte| *byte == 0))
        {
            return Err(InitramfsError::Corrupt("nonzero cpio name padding"));
        }
        let data_end = data_start
            .checked_add(fields[6] as usize)
            .ok_or(InitramfsError::TooLarge)?;
        let data = input
            .get(data_start..data_end)
            .ok_or(InitramfsError::Corrupt("truncated cpio data"))?;
        if checksum
            && data
                .iter()
                .fold(0u32, |acc, byte| acc.wrapping_add(*byte as u32))
                != fields[12]
        {
            return Err(InitramfsError::Corrupt("cpio checksum mismatch"));
        }
        pos = align4(data_end)?;
        if pos > input.len() {
            return Err(InitramfsError::Corrupt("truncated cpio padding"));
        }
        if input[data_end..pos].iter().any(|byte| *byte != 0) {
            return Err(InitramfsError::Corrupt("nonzero cpio data padding"));
        }
        if name == "TRAILER!!!" {
            if !data.is_empty() {
                return Err(InitramfsError::Corrupt("trailer contains data"));
            }
            return Ok((pos, entries));
        }
        consume(Entry {
            name,
            data,
            ino: fields[0],
            mode: fields[1],
            uid: fields[2],
            gid: fields[3],
            nlink: fields[4],
            mtime: fields[5],
            dev_major: fields[7],
            dev_minor: fields[8],
            rdev_major: fields[9],
            rdev_minor: fields[10],
        })?;
        entries += 1;
    }
}

fn align4(position: usize) -> Result<usize, InitramfsError> {
    position
        .checked_add(3)
        .map(|position| position & !3)
        .ok_or(InitramfsError::TooLarge)
}

fn archive_path(name: &str) -> Result<String, InitramfsError> {
    let root_entry = name == "." || name == "./";
    let name = name.trim_start_matches("./");
    if root_entry {
        return Ok(String::from("/"));
    }
    if name.starts_with('/')
        || name.contains('\0')
        || name.split('/').any(|part| part == ".." || part.is_empty())
    {
        return Err(InitramfsError::InvalidPath);
    }
    Ok(alloc::format!("/{name}"))
}

fn apply_entry(
    context: &FsContext,
    entry: Entry<'_>,
    links: &mut BTreeMap<(u32, u32, u32, u8), String>,
) -> Result<(), InitramfsError> {
    let path = archive_path(entry.name)?;
    let ty = NodeType::from(((entry.mode >> 12) & 0xf) as u8);
    if ty == NodeType::Unknown {
        return Err(InitramfsError::Corrupt("unknown cpio file type"));
    }
    let mode = NodePermission::from_bits_truncate(entry.mode as u16);
    let credentials = MutationCredentials::root();
    if path == "/" {
        if ty != NodeType::Directory {
            return Err(InitramfsError::InvalidPath);
        }
        context.root_dir().update_metadata(MetadataUpdate {
            mode: Some(mode),
            owner: Some((entry.uid, entry.gid)),
            mtime: Some(Duration::from_secs(entry.mtime as u64)),
            ..MetadataUpdate::default()
        })?;
        return Ok(());
    }
    let (parent_path, leaf) = path.rsplit_once('/').ok_or(InitramfsError::InvalidPath)?;
    if leaf.is_empty() {
        return Err(InitramfsError::InvalidPath);
    }
    let parent = context.resolve(if parent_path.is_empty() {
        "/"
    } else {
        parent_path
    })?;
    if !parent.is_dir() {
        return Err(InitramfsError::InvalidPath);
    }
    let key = (entry.dev_major, entry.dev_minor, entry.ino, ty as u8);
    let may_link = entry.nlink >= 2 && !matches!(ty, NodeType::Directory | NodeType::Symlink);
    let linked_from = may_link.then(|| links.get(&key).cloned()).flatten();
    let existing = match context.resolve_no_follow(path.as_str()) {
        Ok(location) => Some(location),
        Err(VfsError::NotFound) => None,
        Err(error) => return Err(error.into()),
    };
    let location = if let Some(existing) = existing {
        if ty == NodeType::Directory && existing.is_dir() {
            existing
        } else if ty == NodeType::RegularFile
            && existing.metadata()?.node_type == NodeType::RegularFile
            && linked_from.is_none()
        {
            write_file(context, &path, entry.data, true)?;
            existing
        } else {
            if existing.is_dir() {
                match context.remove_dir(path.as_str(), &credentials) {
                    Ok(()) => {}
                    Err(VfsError::DirectoryNotEmpty) => return Ok(()),
                    Err(error) => return Err(error.into()),
                }
            } else {
                context.remove_file(path.as_str(), &credentials)?;
            }
            create_or_link(
                context,
                &path,
                ty,
                mode,
                &entry,
                linked_from.as_deref(),
                &credentials,
            )?
        }
    } else {
        create_or_link(
            context,
            &path,
            ty,
            mode,
            &entry,
            linked_from.as_deref(),
            &credentials,
        )?
    };
    if may_link && linked_from.is_none() {
        links.insert(key, path.clone());
    }
    if linked_from.is_some() && ty == NodeType::RegularFile && !entry.data.is_empty() {
        write_file(context, &path, entry.data, true)?;
    }
    location.update_metadata(MetadataUpdate {
        mode: Some(mode),
        owner: Some((entry.uid, entry.gid)),
        rdev: matches!(ty, NodeType::CharacterDevice | NodeType::BlockDevice)
            .then(|| DeviceId::new(entry.rdev_major, entry.rdev_minor)),
        mtime: Some(Duration::from_secs(entry.mtime as u64)),
        ..MetadataUpdate::default()
    })?;
    Ok(())
}

fn create_or_link(
    context: &FsContext,
    path: &str,
    ty: NodeType,
    mode: NodePermission,
    entry: &Entry<'_>,
    previous: Option<&str>,
    credentials: &MutationCredentials<'_>,
) -> Result<axfs_ng_vfs::Location, InitramfsError> {
    if let Some(previous) = previous {
        return Ok(context.link(previous, path, credentials)?);
    }
    create_entry(context, path, ty, mode, entry, credentials)
}

fn write_file(
    context: &FsContext,
    path: &str,
    data: &[u8],
    truncate: bool,
) -> Result<(), InitramfsError> {
    let file = OpenOptions::new()
        .write(true)
        .truncate(truncate)
        .open(context, path)?
        .into_file()?;
    let written = file.write_at(data, 0)?;
    if written != data.len() {
        return Err(InitramfsError::Corrupt("short initramfs write"));
    }
    Ok(())
}

fn create_entry(
    context: &FsContext,
    path: &str,
    ty: NodeType,
    mode: NodePermission,
    entry: &Entry<'_>,
    credentials: &MutationCredentials<'_>,
) -> Result<axfs_ng_vfs::Location, InitramfsError> {
    let location = match ty {
        NodeType::Directory => context.create_dir(path, mode, entry.uid, entry.gid, credentials)?,
        NodeType::Symlink => {
            if entry.data.contains(&0) {
                return Err(InitramfsError::Corrupt("NUL in symlink target"));
            }
            let target = str::from_utf8(entry.data).map_err(|_| InitramfsError::InvalidText)?;
            context.symlink(target, path, entry.uid, entry.gid, credentials)?
        }
        _ => context.create_node(path, ty, mode, entry.uid, entry.gid, credentials)?,
    };
    if ty == NodeType::RegularFile && !entry.data.is_empty() {
        write_file(context, path, entry.data, false)?;
    }
    Ok(location)
}

fn inflate_gzip(input: &[u8]) -> Result<(Vec<u8>, usize), InitramfsError> {
    if input.len() < 18 || input[..2] != [0x1f, 0x8b] || input[2] != 8 || input[3] & 0xe0 != 0 {
        return Err(InitramfsError::Corrupt("invalid gzip header"));
    }
    let flags = input[3];
    let mut offset = 10;
    if flags & 4 != 0 {
        let length = input
            .get(offset..offset + 2)
            .ok_or(InitramfsError::Corrupt("truncated gzip extra"))?;
        offset += 2 + u16::from_le_bytes(length.try_into().unwrap()) as usize;
        if offset > input.len() {
            return Err(InitramfsError::Corrupt("truncated gzip extra"));
        }
    }
    for flag in [8, 16] {
        if flags & flag != 0 {
            offset += input
                .get(offset..)
                .ok_or(InitramfsError::Corrupt("truncated gzip text"))?
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(InitramfsError::Corrupt("unterminated gzip text"))?
                + 1;
        }
    }
    if flags & 2 != 0 {
        let header_crc = input
            .get(offset..offset + 2)
            .ok_or(InitramfsError::Corrupt("truncated gzip header checksum"))?;
        if (crc32(&input[..offset]) as u16).to_le_bytes() != header_crc {
            return Err(InitramfsError::Corrupt("gzip header checksum mismatch"));
        }
        offset += 2;
    }
    if input.len().saturating_sub(offset) < 8 {
        return Err(InitramfsError::Corrupt("truncated gzip body"));
    }
    let mut decoder = InflateState::new_boxed(DataFormat::Raw);
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let result = inflate(&mut decoder, &input[offset..], &mut buffer, MZFlush::None);
        offset += result.bytes_consumed;
        if output
            .len()
            .checked_add(result.bytes_written)
            .is_none_or(|len| len > MAX_INFLATED_SIZE)
        {
            return Err(InitramfsError::TooLarge);
        }
        output.extend_from_slice(&buffer[..result.bytes_written]);
        if result.status == Ok(MZStatus::StreamEnd) {
            break;
        }
        if result.status.is_err() || result.bytes_consumed + result.bytes_written == 0 {
            return Err(InitramfsError::Corrupt("invalid gzip deflate stream"));
        }
    }
    let trailer = input
        .get(offset..offset + 8)
        .ok_or(InitramfsError::Corrupt("missing gzip trailer"))?;
    let crc = u32::from_le_bytes(trailer[..4].try_into().unwrap());
    let size = u32::from_le_bytes(trailer[4..].try_into().unwrap());
    if crc32(&output) != crc || output.len() as u32 != size {
        return Err(InitramfsError::Corrupt("gzip checksum mismatch"));
    }
    Ok((output, offset + 8))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::memory::test_support::with_test_page_provider;

    fn add_entry(
        archive: &mut Vec<u8>,
        name: &str,
        mode: u32,
        ino: u32,
        nlink: u32,
        data: &[u8],
        crc: bool,
    ) {
        let fields = [
            ino,
            mode,
            12,
            34,
            nlink,
            42,
            data.len() as u32,
            0,
            0,
            0,
            0,
            name.len() as u32 + 1,
            if crc {
                data.iter()
                    .fold(0u32, |sum, byte| sum.wrapping_add(*byte as u32))
            } else {
                0
            },
        ];
        archive.extend_from_slice(if crc { b"070702" } else { b"070701" });
        for field in fields {
            archive.extend_from_slice(alloc::format!("{field:08x}").as_bytes());
        }
        archive.extend_from_slice(name.as_bytes());
        archive.push(0);
        while !archive.len().is_multiple_of(4) {
            archive.push(0);
        }
        archive.extend_from_slice(data);
        while !archive.len().is_multiple_of(4) {
            archive.push(0);
        }
    }

    fn finish(archive: &mut Vec<u8>, crc: bool) {
        add_entry(archive, "TRAILER!!!", 0, 0, 1, &[], crc);
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut compressed = Vec::from(&b"\x1f\x8b\x08\x02\x00\x00\x00\x00\x00\x03"[..]);
        compressed.extend_from_slice(&(crc32(&compressed) as u16).to_le_bytes());
        let zlib = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
        compressed.extend_from_slice(&zlib[2..zlib.len() - 4]);
        compressed.extend_from_slice(&crc32(data).to_le_bytes());
        compressed.extend_from_slice(&(data.len() as u32).to_le_bytes());
        compressed
    }

    #[test]
    fn concatenation_links_permissions_and_overlay() {
        with_test_page_provider(true, |_| {
            let mut built_in = Vec::new();
            add_entry(&mut built_in, ".", 0o040755, 1, 2, &[], false);
            add_entry(&mut built_in, "bin", 0o040755, 2, 2, &[], false);
            add_entry(&mut built_in, "bin-alias", 0o120777, 6, 2, b"bin", false);
            add_entry(&mut built_in, "bin-alias-2", 0o120777, 6, 2, b"bin", false);
            add_entry(
                &mut built_in,
                "bin-alias/through",
                0o100644,
                7,
                1,
                b"linked",
                false,
            );
            add_entry(&mut built_in, "bin/init", 0o100755, 3, 2, b"hello", false);
            add_entry(&mut built_in, "bin/init-copy", 0o100755, 3, 2, &[], false);
            add_entry(&mut built_in, "short-a", 0o100644, 8, 2, b"ABCDEFGH", false);
            add_entry(&mut built_in, "short-b", 0o100644, 8, 2, b"WXYZ", false);
            finish(&mut built_in, false);

            let mut external = Vec::new();
            add_entry(&mut external, "bin", 0o100644, 9, 1, b"blocked", true);
            add_entry(&mut external, "bin/init", 0o100755, 5, 1, b"updated", true);
            add_entry(&mut external, "init", 0o120777, 4, 1, b"bin/init", true);
            finish(&mut external, true);
            let mut combined = built_in.clone();
            combined.extend_from_slice(&[0; 8]);
            combined.extend_from_slice(&external);

            let (fs, report) = unpack_sources(&[&combined, &external]).unwrap();
            assert_eq!(
                report,
                UnpackReport {
                    archives: 3,
                    entries: 15
                }
            );
            let context = FsContext::new(Mountpoint::new_root(&fs).root_location());
            let init = context.metadata("/bin/init").unwrap();
            let copy = context.metadata("/bin/init-copy").unwrap();
            assert_eq!(init.inode, copy.inode);
            assert_eq!(init.nlink, 2);
            assert_eq!(init.mode.bits() & 0o777, 0o755);
            assert_eq!((init.uid, init.gid), (12, 34));
            assert!(context.resolve("/bin").unwrap().is_dir());
            let short = OpenOptions::new()
                .read(true)
                .open(&context, "/short-a")
                .unwrap()
                .into_file()
                .unwrap();
            let mut bytes = [0; 8];
            assert_eq!(short.read_at(&mut bytes[..], 0).unwrap(), 4);
            assert_eq!(&bytes[..4], b"WXYZ");
            assert_eq!(context.metadata("/bin/through").unwrap().size, 6);
            assert_eq!(
                context
                    .resolve_no_follow("/init")
                    .unwrap()
                    .read_link()
                    .unwrap(),
                "bin/init"
            );
            let alias = context.resolve_no_follow("/bin-alias-2").unwrap();
            assert_eq!(alias.metadata().unwrap().node_type, NodeType::Symlink);
            assert_eq!(alias.read_link().unwrap(), "bin");
        });
    }

    #[test]
    fn gzip_and_rejection_cases() {
        with_test_page_provider(true, |_| {
            let mut archive = Vec::new();
            add_entry(&mut archive, "init", 0o100755, 1, 1, b"hello", true);
            let (_, without_trailer) = unpack(&archive).unwrap();
            assert_eq!(without_trailer.entries, 1);
            finish(&mut archive, true);
            let compressed = gzip(&archive);
            let (_, report) = unpack(&compressed).unwrap();
            assert_eq!(
                report,
                UnpackReport {
                    archives: 1,
                    entries: 1
                }
            );
            let mut concatenated = compressed.clone();
            concatenated.extend_from_slice(&compressed);
            assert_eq!(unpack(&concatenated).unwrap().1.archives, 2);
            assert!(matches!(
                unpack(&gzip(&compressed)),
                Err(InitramfsError::Corrupt("nested gzip archive"))
            ));
            let mut damaged = compressed.clone();
            *damaged.last_mut().unwrap() ^= 1;
            assert!(matches!(unpack(&damaged), Err(InitramfsError::Corrupt(_))));
            assert!(matches!(
                unpack(b"\xfd7zXZ"),
                Err(InitramfsError::UnsupportedCompression)
            ));

            let mut bad_path = Vec::new();
            add_entry(&mut bad_path, "../escape", 0o100644, 1, 1, &[], false);
            finish(&mut bad_path, false);
            assert!(matches!(
                unpack(&bad_path),
                Err(InitramfsError::InvalidPath)
            ));

            let mut bad_symlink = Vec::new();
            add_entry(
                &mut bad_symlink,
                "link",
                0o120777,
                1,
                1,
                b"bin\0/escape",
                false,
            );
            finish(&mut bad_symlink, false);
            assert!(matches!(
                unpack(&bad_symlink),
                Err(InitramfsError::Corrupt("NUL in symlink target"))
            ));

            let mut damaged_crc = archive;
            let data_offset = damaged_crc
                .windows(5)
                .position(|bytes| bytes == b"hello")
                .unwrap();
            damaged_crc[data_offset] ^= 1;
            assert!(matches!(
                unpack(&damaged_crc),
                Err(InitramfsError::Corrupt(_))
            ));
            assert!(matches!(
                unpack_sources(&[&compressed, &[0; 4]]),
                Err(InitramfsError::Corrupt(_))
            ));
        });
    }
}

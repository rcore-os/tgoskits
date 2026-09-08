//! Byte-bounded parser for the `newc` CPIO archive format.
//!
//! Kept free of platform dependencies (only `core`/`alloc`) so it can be
//! unit-tested on any host (`cargo test -p arceboot-cpio`).
#![no_std]

extern crate alloc;

use alloc::{format, vec::Vec};
use core::str;

const CPIO_MAGIC: &[u8; 6] = b"070701";
const TRAILER_NAME: &str = "TRAILER!!!";
/// `struct cpio_newc_header`: 6 bytes magic + 13 eight-digit hex fields.
const HDR_SIZE: usize = 110;

/// Archive parsing failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The archive ended in the middle of a record.
    Truncated,
    /// A header field did not parse or is out of range.
    Invalid,
}

/// Parses one eight-digit ASCII hex header field.
fn parse_hex_field(field: &[u8]) -> usize {
    let s = core::str::from_utf8(field).unwrap_or("0");
    usize::from_str_radix(s, 16).unwrap_or(0)
}

fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

/// Walks the entries of a `newc` archive, invoking `f` for every entry until
/// it returns `true` (match found).
///
/// Every header, name, and file-data range is bounds-checked through slice
/// indexing, so a truncated or hostile archive yields [`Error::Truncated`] /
/// [`Error::Invalid`] instead of an out-of-bounds access. Returns the total
/// archive length (through the trailer record), which is the real ramdisk
/// size the DTB `linux,initrd-end` property must be computed from.
pub fn walk_archive<'a>(
    archive: &'a [u8],
    mut f: impl FnMut(&str, &'a [u8]) -> bool,
) -> Result<usize, Error> {
    let mut off = 0usize;

    loop {
        let hdr = archive.get(off..off + HDR_SIZE).ok_or(Error::Truncated)?;
        if &hdr[..6] != CPIO_MAGIC {
            return Err(Error::Invalid);
        }

        let namesize = parse_hex_field(&hdr[94..102]);
        let filesize = parse_hex_field(&hdr[54..62]);
        // The name is NUL-terminated within `namesize`; without this check a
        // zero `namesize` would underflow the `namesize - 1` below.
        if namesize == 0 {
            return Err(Error::Invalid);
        }

        let name_off = off + HDR_SIZE;
        let name = archive
            .get(name_off..name_off + namesize - 1)
            .and_then(|slice| str::from_utf8(slice).ok())
            .unwrap_or("<invalid utf8>");

        let data_off = align_up(name_off + namesize, 4);
        // The record is padded to a 4-byte boundary; a final record may end
        // exactly at the archive end without its padding bytes.
        let data_end = data_off.checked_add(filesize).ok_or(Error::Invalid)?;
        let next = align_up(data_end, 4);
        if next > archive.len() && data_end != archive.len() {
            return Err(Error::Truncated);
        }
        let data = archive
            .get(data_off..data_off + filesize)
            .ok_or(Error::Truncated)?;

        if name == TRAILER_NAME || f(name, data) {
            return Ok(next.min(archive.len()));
        }

        off = next;
    }
}

fn path_matches(path: &str, name: &str) -> bool {
    if path.starts_with('/') {
        &path[1..] == name
    } else {
        path == name
    }
}

/// Finds the entry matching `path` (with or without a leading `/`) and
/// returns its data, or `Ok(None)` if the archive has no such entry.
pub fn find_entry<'a>(archive: &'a [u8], path: &str) -> Result<Option<&'a [u8]>, Error> {
    let mut found: Option<&[u8]> = None;
    walk_archive(archive, |name, data| {
        if path_matches(path, name) {
            found = Some(data);
            true
        } else {
            false
        }
    })?;
    Ok(found)
}

/// Builds one `newc` record (header + padded name + padded data); used by the
/// tests and handy for ad-hoc archive construction.
pub fn make_record(name: &str, data: &[u8]) -> Vec<u8> {
    fn field(rec: &mut Vec<u8>, val: usize) {
        rec.extend_from_slice(format!("{:08X}", val).as_bytes());
    }
    let mut rec = Vec::new();
    rec.extend_from_slice(b"070701");
    for _ in 0..6 {
        field(&mut rec, 0); // ino, mode, uid, gid, nlink, mtime
    }
    field(&mut rec, data.len()); // filesize
    for _ in 0..4 {
        field(&mut rec, 0); // devmajor, devminor, rdevmajor, rdevminor
    }
    field(&mut rec, name.len() + 1); // namesize includes the trailing NUL
    field(&mut rec, 0); // check
    rec.extend_from_slice(name.as_bytes());
    rec.push(0);
    rec.resize(align_up(rec.len(), 4), 0);
    rec.extend_from_slice(data);
    rec.resize(align_up(rec.len(), 4), 0);
    rec
}

#[cfg(test)]
mod tests {
    use alloc::{string::ToString, vec};

    use super::*;

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        for (name, data) in entries {
            buf.extend_from_slice(&make_record(name, data));
        }
        buf.extend_from_slice(&make_record(TRAILER_NAME, b""));
        buf
    }

    #[test]
    fn walks_valid_archive_and_derives_total_length() {
        let buf = archive(&[
            ("EFI/BOOT/BOOTRISCV64.EFI", b"[pe bytes]"),
            ("test.txt", b"hello"),
        ]);
        let mut seen = vec![];
        let total = walk_archive(&buf, |name, _| {
            seen.push(name.to_string());
            false
        })
        .unwrap();
        assert_eq!(seen, vec!["EFI/BOOT/BOOTRISCV64.EFI", "test.txt"]);
        assert_eq!(total, buf.len());
    }

    #[test]
    fn finds_entry_data() {
        let buf = archive(&[("hello.txt", b"payload")]);
        assert_eq!(
            find_entry(&buf, "hello.txt").unwrap(),
            Some(&b"payload"[..])
        );
        // A leading slash is tolerated.
        assert_eq!(
            find_entry(&buf, "/hello.txt").unwrap(),
            Some(&b"payload"[..])
        );
        assert_eq!(find_entry(&buf, "missing").unwrap(), None);
    }

    #[test]
    fn truncated_header_is_rejected() {
        let buf = archive(&[("a", b"data")]);
        // Cut everything: even the first header no longer fits.
        let err = walk_archive(&buf[..4], |_, _| false).unwrap_err();
        assert_eq!(err, Error::Truncated);
    }

    #[test]
    fn truncated_file_data_is_rejected() {
        let mut buf = archive(&[("a", b"0123456789")]);
        // Keep only the bytes through the name; the file data is gone.
        let cut = HDR_SIZE + align_up("a".len() + 1, 4);
        buf.truncate(cut);
        let err = walk_archive(&buf, |_, _| false).unwrap_err();
        assert_eq!(err, Error::Truncated);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut buf = vec![0u8; HDR_SIZE];
        buf[..6].copy_from_slice(b"XXXXXX");
        let err = walk_archive(&buf, |_, _| false).unwrap_err();
        assert_eq!(err, Error::Invalid);
    }

    #[test]
    fn zero_name_size_is_rejected() {
        let mut buf = vec![0u8; HDR_SIZE];
        buf[..6].copy_from_slice(b"070701");
        buf[94..102].copy_from_slice(b"00000000"); // namesize = 0
        let err = walk_archive(&buf, |_, _| false).unwrap_err();
        assert_eq!(err, Error::Invalid);
    }

    #[test]
    fn filesize_past_archive_end_is_rejected() {
        let mut buf = vec![0u8; HDR_SIZE + 8];
        buf[..6].copy_from_slice(b"070701");
        buf[54..62].copy_from_slice(b"0000FFFF"); // filesize = 0xffff
        buf[94..102].copy_from_slice(b"00000002"); // namesize = 2 ("a\0")
        buf[HDR_SIZE] = b'a';
        let err = walk_archive(&buf, |_, _| false).unwrap_err();
        assert_eq!(err, Error::Truncated);
    }
}

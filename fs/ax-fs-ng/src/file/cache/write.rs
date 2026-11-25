//! Resident input staging separated from physical cache publication.

use alloc::vec::Vec;
use core::io::BorrowedBuf;

use ax_io::{IoBuf, Read};
use axfs_ng_vfs::{FileNode, VfsError, VfsResult};

use super::{CachedFile, PAGE_SIZE, PageCache};

const WRITE_BATCH_PAGES: usize = 16;
const WRITE_BATCH_BYTES: usize = WRITE_BATCH_PAGES * PAGE_SIZE;

/// Unpublished frame candidates remain owned outside every cache guard.
/// A candidate that loses to a read fill is reused by a later input batch.
struct ResidentWriteBuffer {
    bytes: Vec<u8>,
    pages: [Option<PageCache>; WRITE_BATCH_PAGES],
}

enum WritePosition {
    Offset(u64),
    Append,
}

impl CachedFile {
    /// Writes `buf` to the file at `offset`.
    pub fn write_at(&self, buf: impl Read + IoBuf, offset: u64) -> VfsResult<usize> {
        self.write_at_with_completion(buf, offset, |_| Ok(()))
    }

    /// Runs completion after releasing cache locks while retaining write admission.
    pub(crate) fn write_at_with_completion(
        &self,
        buf: impl Read + IoBuf,
        offset: u64,
        complete: impl FnOnce(usize) -> VfsResult<()>,
    ) -> VfsResult<usize> {
        self.write_with_completion(buf, WritePosition::Offset(offset), complete)
            .map(|(written, _)| written)
    }

    /// Appends `buf` to the end of the file. Returns `(bytes_written, new_end)`.
    pub fn append(&self, buf: impl Read + IoBuf) -> VfsResult<(usize, u64)> {
        self.append_with_completion(buf, |_| Ok(()))
    }

    /// Runs completion after releasing cache locks while retaining write admission.
    pub(crate) fn append_with_completion(
        &self,
        buf: impl Read + IoBuf,
        complete: impl FnOnce(usize) -> VfsResult<()>,
    ) -> VfsResult<(usize, u64)> {
        self.write_with_completion(buf, WritePosition::Append, complete)
    }

    fn write_with_completion(
        &self,
        mut source: impl Read + IoBuf,
        position: WritePosition,
        complete: impl FnOnce(usize) -> VfsResult<()>,
    ) -> VfsResult<(usize, u64)> {
        // Neither owned input nor staging may retire under cache protection.
        let mut staging = ResidentWriteBuffer {
            bytes: Vec::new(),
            pages: [const { None }; WRITE_BATCH_PAGES],
        };
        let result = (|| {
            let _write =
                axfs_ng_vfs::CachedWriteGuard::acquire(self.inner.location().filesystem())?;
            self.shared.ensure_writeback_owner_active()?;
            if !source.is_empty() {
                let size = source.remaining().min(WRITE_BATCH_BYTES);
                staging
                    .bytes
                    .try_reserve_exact(size)
                    .map_err(|_| VfsError::NoMemory)?;
                staging.bytes.resize(size, 0);
            }
            let (written, end) = {
                let _content = self.shared.lock_content_operation();
                self.shared.ensure_writeback_owner_active()?;
                let offset = match position {
                    WritePosition::Offset(offset) => offset,
                    WritePosition::Append => self.shared.len(),
                };
                let written = self.write_source(&mut source, offset, &mut staging)?;
                (written, offset + written as u64)
            };
            complete(written)?;
            Ok((written, end))
        })();
        drop(staging);
        result
    }

    fn write_source(
        &self,
        source: &mut (impl Read + IoBuf),
        offset: u64,
        staging: &mut ResidentWriteBuffer,
    ) -> VfsResult<usize> {
        let ResidentWriteBuffer {
            bytes: scratch,
            pages,
        } = staging;
        if scratch.is_empty() {
            return Ok(0);
        }
        let file = self.inner.location().entry().as_file()?;
        let end = offset.saturating_add(source.remaining() as u64);
        let mut written = 0;
        let mut current = offset;
        while current < end && !source.is_empty() {
            let page_offset = (current % PAGE_SIZE as u64) as usize;
            let chunk_len = (WRITE_BATCH_BYTES - page_offset)
                .min(scratch.len())
                .min(source.remaining())
                .min((end - current) as usize);
            let mut staged = BorrowedBuf::from(&mut scratch[..chunk_len]);
            // Input can fault or read this cache. Logical writer exclusion is
            // retained, but no layout, I/O, index or persistence guard is held.
            let error = source
                .read_buf(staged.unfilled())
                .map_err(crate::io_error_to_vfs_error)
                .err();
            let count = staged.len();
            if count == 0 {
                if let Some(error) = error
                    && written == 0
                {
                    return Err(error);
                }
                break;
            }

            let grew = self.write_resident_batch(file, staged.filled(), current, pages)?;
            written += count;
            current += count as u64;
            if !self.in_memory {
                self.shared.balance_dirty_pages(grew)?;
            }
            if error.is_some() {
                break;
            }
        }
        Ok(written)
    }

    fn write_resident_batch(
        &self,
        file: &FileNode,
        bytes: &[u8],
        offset: u64,
        prepared: &mut [Option<PageCache>; WRITE_BATCH_PAGES],
    ) -> VfsResult<bool> {
        let _backing_change = (!self.in_memory)
            .then(|| self.shared.backing_persistence.begin())
            .transpose()?;
        let _mapping_update = self.begin_mapping_update()?;
        let old_len = self.shared.len();
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or(VfsError::FileTooLarge)?;
        let grew = end > old_len;
        let _writeback = grew.then(|| self.shared.writeback_lock.lock());
        let _update = self.shared.lock_for_update();
        let next_epoch = grew
            .then(|| self.shared.prepare_mapping_epoch())
            .transpose()?;
        if grew {
            if !old_len.is_multiple_of(PAGE_SIZE as u64) {
                let page_number = (old_len / PAGE_SIZE as u64) as u32;
                let page_start = u64::from(page_number) * PAGE_SIZE as u64;
                self.zero_partial_page_locked(
                    file,
                    page_number,
                    (old_len - page_start) as usize,
                    (end - page_start).min(PAGE_SIZE as u64) as usize,
                )?;
            }
        }
        // The old partial tail can install its own page. Prepare after that
        // step, then retain layout and I/O exclusion until every copy commits.
        // Failed frame allocation cannot publish a larger native file length.
        self.prepare_resident_pages(file, bytes, offset, old_len, prepared)?;
        if let Some(next_epoch) = next_epoch {
            file.set_len(end)?;
            self.shared.update_len_max(end);
            self.shared.publish_mapping_epoch(next_epoch);
        }
        let mut consumed = 0;
        for candidate in prepared {
            if consumed == bytes.len() {
                break;
            }
            let current = offset + consumed as u64;
            let number =
                u32::try_from(current / PAGE_SIZE as u64).map_err(|_| VfsError::FileTooLarge)?;
            let page_offset = (current % PAGE_SIZE as u64) as usize;
            let count = (PAGE_SIZE - page_offset).min(bytes.len() - consumed);
            let mut cache = self.shared.page_cache.lock();
            if let Some(mut page) = cache.get_mut(&number) {
                page.data()[page_offset..page_offset + count]
                    .copy_from_slice(&bytes[consumed..consumed + count]);
                if !self.in_memory {
                    page.mark_dirty();
                }
            } else {
                let mut page = candidate.take().ok_or(VfsError::BadState)?;
                page.data()[page_offset..page_offset + count]
                    .copy_from_slice(&bytes[consumed..consumed + count]);
                if !self.in_memory {
                    page.mark_dirty();
                }
                *candidate = cache.put(number, page);
                debug_assert!(
                    candidate.is_none(),
                    "write publication holds index exclusion"
                );
            }
            consumed += count;
        }
        debug_assert_eq!(consumed, bytes.len());
        Ok(grew)
    }

    fn prepare_resident_pages(
        &self,
        file: &FileNode,
        bytes: &[u8],
        offset: u64,
        old_len: u64,
        prepared: &mut [Option<PageCache>; WRITE_BATCH_PAGES],
    ) -> VfsResult<()> {
        let mut consumed = 0;
        for candidate in prepared {
            if consumed == bytes.len() {
                break;
            }
            let current = offset + consumed as u64;
            let number =
                u32::try_from(current / PAGE_SIZE as u64).map_err(|_| VfsError::FileTooLarge)?;
            let page_start = u64::from(number) * PAGE_SIZE as u64;
            let page_offset = (current - page_start) as usize;
            let count = (PAGE_SIZE - page_offset).min(bytes.len() - consumed);
            if !self.shared.page_cache.lock().contains(&number) {
                if candidate.is_none() {
                    *candidate = Some(PageCache::new()?);
                }
                let read_backing =
                    page_start < old_len && !(page_offset == 0 && count == PAGE_SIZE);
                self.initialize_cache_page(
                    file,
                    number,
                    read_backing,
                    candidate.as_mut().ok_or(VfsError::BadState)?,
                )?;
            }
            consumed += count;
        }
        debug_assert_eq!(consumed, bytes.len());
        Ok(())
    }
}

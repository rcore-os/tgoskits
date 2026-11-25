//! Resident input staging separated from physical cache publication.

use alloc::vec::Vec;
use core::io::BorrowedBuf;

use ax_io::{IoBuf, Read};
use axfs_ng_vfs::{FileNode, VfsError, VfsResult};

use super::{CachedFile, PAGE_SIZE};

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
        let mut scratch = Vec::new();
        let result = (|| {
            let _write =
                axfs_ng_vfs::CachedWriteGuard::acquire(self.inner.location().filesystem())?;
            self.shared.ensure_writeback_owner_active()?;
            if !source.is_empty() {
                scratch
                    .try_reserve_exact(PAGE_SIZE)
                    .map_err(|_| VfsError::NoMemory)?;
                scratch.resize(PAGE_SIZE, 0);
            }
            let (written, end) = {
                let _content = self.shared.lock_content_operation();
                self.shared.ensure_writeback_owner_active()?;
                let offset = match position {
                    WritePosition::Offset(offset) => offset,
                    WritePosition::Append => self.shared.len(),
                };
                let written = self.write_source(&mut source, offset, &mut scratch)?;
                (written, offset + written as u64)
            };
            complete(written)?;
            Ok((written, end))
        })();
        drop(scratch);
        result
    }

    fn write_source(
        &self,
        source: &mut (impl Read + IoBuf),
        offset: u64,
        scratch: &mut [u8],
    ) -> VfsResult<usize> {
        if scratch.is_empty() {
            return Ok(0);
        }
        let file = self.inner.location().entry().as_file()?;
        let end = offset.saturating_add(source.remaining() as u64);
        let mut written = 0;
        let mut current = offset;
        while current < end && !source.is_empty() {
            let page_offset = (current % PAGE_SIZE as u64) as usize;
            let chunk_len = (PAGE_SIZE - page_offset)
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

            let grew = self.write_resident_chunk(file, staged.filled(), current)?;
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

    fn write_resident_chunk(&self, file: &FileNode, bytes: &[u8], offset: u64) -> VfsResult<bool> {
        let _backing_change = (!self.in_memory)
            .then(|| self.shared.backing_persistence.begin())
            .transpose()?;
        let _mapping_update = self.begin_mapping_update()?;
        let mut update = self.shared.lock_for_update();
        let old_len = self.shared.len();
        let end = offset + bytes.len() as u64;
        let grew = end > old_len;
        if grew {
            update.without_io(|| -> VfsResult<()> {
                let _writeback = self.shared.writeback_lock.lock();
                let _update = self.shared.lock_for_update();
                let next_epoch = self.shared.prepare_mapping_epoch()?;
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
                file.set_len(end)?;
                self.shared.update_len_max(end);
                self.shared.publish_mapping_epoch(next_epoch);
                Ok(())
            })?;
        }
        let number = (offset / PAGE_SIZE as u64) as u32;
        let page_start = u64::from(number) * PAGE_SIZE as u64;
        let page_offset = (offset - page_start) as usize;
        let read_backing = page_start < old_len && !(page_offset == 0 && bytes.len() == PAGE_SIZE);
        self.with_page_or_insert(file, number, read_backing, |page, _| {
            page.data()[page_offset..page_offset + bytes.len()].copy_from_slice(bytes);
            if !self.in_memory {
                page.mark_dirty();
            }
        })?;
        Ok(grew)
    }
}

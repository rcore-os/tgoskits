//! One page traversal with separate resident and faultable destinations.

use core::io::{BorrowedBuf, BorrowedCursor};

use ax_io::{IoBufMut, Write};
use axfs_ng_vfs::{FileNode, VfsResult};

use super::{CachedFile, PAGE_SIZE, PageCache};

/// Request-local position and readahead; no cache lock survives a page copy.
struct CachedRead<'a> {
    cached: &'a CachedFile,
    file: &'a FileNode,
    current: u64,
    end: u64,
    window_pages: usize,
}

impl CachedFile {
    /// Reads into a resident kernel buffer without an intermediate page.
    ///
    /// The cursor's storage must remain resident and exclusively borrowed for
    /// this call. Use [`Self::read_at`] for faultable user-memory Writers.
    /// Returns this call's progress, excluding previously filled cursor bytes.
    /// Allocation or backing-I/O errors may leave a filled prefix in `dst`.
    pub fn read_buf_at(&self, mut dst: BorrowedCursor<'_, u8>, offset: u64) -> VfsResult<usize> {
        let Some(mut reader) = CachedRead::new(self, offset, dst.capacity())? else {
            return Ok(0);
        };
        let mut total = 0;
        while reader.current < reader.end {
            total += reader.read_page(dst.reborrow())?;
        }
        Ok(total)
    }

    /// Reads data from the file at `offset` into `dst`.
    pub fn read_at(&self, mut dst: impl Write + IoBufMut, offset: u64) -> VfsResult<usize> {
        let Some(mut reader) = CachedRead::new(self, offset, dst.remaining_mut())? else {
            return Ok(0);
        };
        let mut scratch = PageCache::new()?;
        let mut total = 0;
        while reader.current < reader.end {
            let mut bytes = scratch.data();
            let mut snapshot = BorrowedBuf::from(&mut *bytes);
            let copied = reader.read_page(snapshot.unfilled())?;
            // A Writer may fault, block, or reenter this cache. read_page has
            // released every cache lock, and short writes consume this same
            // stable snapshot before the next page is fetched.
            dst.write_all(snapshot.filled())
                .map_err(crate::io_error_to_vfs_error)?;
            total += copied;
        }
        Ok(total)
    }
}

impl<'a> CachedRead<'a> {
    fn new(cached: &'a CachedFile, offset: u64, capacity: usize) -> VfsResult<Option<Self>> {
        let end = offset
            .saturating_add(capacity as u64)
            .min(cached.shared.len());
        if end <= offset {
            return Ok(None);
        }
        let window_pages = if cached.in_memory {
            1
        } else {
            cached.readahead.lock().plan(offset, end).window_pages
        };
        Ok(Some(Self {
            cached,
            file: cached.inner.entry().as_file()?,
            current: offset,
            end,
            window_pages,
        }))
    }

    fn read_page(&mut self, mut dst: BorrowedCursor<'_, u8>) -> VfsResult<usize> {
        let page_number = (self.current / PAGE_SIZE as u64) as u32;
        let page_start = u64::from(page_number) * PAGE_SIZE as u64;
        let page_offset = (self.current - page_start) as usize;
        let copied = ((self.end - self.current) as usize)
            .min(PAGE_SIZE - page_offset)
            .min(dst.capacity());
        let source = page_offset..page_offset + copied;

        let mut filled = false;
        loop {
            if let Some(copied) =
                self.cached
                    .shared
                    .try_copy_cached_page(page_number, source.clone(), dst.reborrow())
            {
                self.current += copied as u64;
                if copied == 0 {
                    self.end = self.current;
                }
                if filled {
                    self.cached.trim_clean_pages_after_read();
                }
                return Ok(copied);
            }
            match self
                .cached
                .populate_page_window(self.file, page_number, self.window_pages)
            {
                Ok(()) => filled = true,
                Err(axfs_ng_vfs::VfsError::ResourceBusy) => {
                    // Ordinary buffered readers may wait for a layout mutation;
                    // fault preparation uses the same fill API and retries in MM.
                    drop(self.cached.shared.mapping_layout_lock.lock());
                }
                Err(error) => return Err(error),
            }
        }
    }
}

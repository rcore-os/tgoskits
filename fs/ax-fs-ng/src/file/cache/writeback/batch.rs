//! Owned, bounded contiguous bytes spanning backing I/O without cache locks.

use super::*;

const MAX_BATCH_PAGES: usize = 256;

struct WritebackBatch {
    offset: u64,
    bytes: Vec<u8>,
    versions: Vec<PageVersion>,
}

struct PageVersion {
    selection_index: usize,
    generation: u64,
}

impl WritebackPages<'_> {
    pub(super) fn write_back(&self) -> VfsResult<()> {
        let mut cursor = 0;
        while let Some(batch) = self.prepare_batch(&mut cursor)? {
            super::super::backing::write_all_at(
                self.shared.backing()?,
                &batch.bytes,
                batch.offset,
            )?;
            self.complete_batch(&batch);
        }
        Ok(())
    }

    fn prepare_batch(&self, cursor: &mut usize) -> VfsResult<Option<WritebackBatch>> {
        let capacity = (self.pages.len() - *cursor).min(MAX_BATCH_PAGES);
        if capacity == 0 {
            return Ok(None);
        }
        let mut batch = WritebackBatch::new(capacity)?;
        let _io = self.shared.io_lock.lock();
        let mut cache = self.shared.page_cache.lock();
        while *cursor < self.pages.len() && batch.versions.len() < MAX_BATCH_PAGES {
            let tracked = &self.pages[*cursor];
            if !tracked.protected {
                if batch.bytes.is_empty() {
                    *cursor += 1;
                    continue;
                }
                break;
            }
            let current = cache
                .get_mut(&tracked.number)
                .filter(|page| page.paddr() == Ok(tracked.paddr) && page.dirty);
            let Some(page) = current else {
                if batch.bytes.is_empty() {
                    *cursor += 1;
                    continue;
                }
                break;
            };
            let offset = u64::from(tracked.number) * PAGE_SIZE as u64;
            if batch.bytes.is_empty() {
                batch.offset = offset;
            } else if offset != batch.offset + batch.bytes.len() as u64 {
                break;
            }
            let len = self.file_len.saturating_sub(offset).min(PAGE_SIZE as u64) as usize;
            // The fixed selection excludes pages at/after its captured EOF.
            debug_assert!(len != 0);
            batch.bytes.extend_from_slice(&page.data()[..len]);
            batch.versions.push(PageVersion {
                selection_index: *cursor,
                generation: page.dirty_generation,
            });
            *cursor += 1;
        }
        Ok((!batch.bytes.is_empty()).then_some(batch))
    }

    fn complete_batch(&self, batch: &WritebackBatch) {
        let _io = self.shared.io_lock.lock();
        let mut cache = self.shared.page_cache.lock();
        for version in &batch.versions {
            let tracked = &self.pages[version.selection_index];
            if let Some(page) = cache.get_mut(&tracked.number)
                && page.paddr() == Ok(tracked.paddr)
            {
                page.complete_writeback(version.generation);
            }
        }
    }
}

impl WritebackBatch {
    fn new(pages: usize) -> VfsResult<Self> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(pages * PAGE_SIZE)
            .map_err(|_| VfsError::NoMemory)?;
        let mut versions = Vec::new();
        versions
            .try_reserve_exact(pages)
            .map_err(|_| VfsError::NoMemory)?;
        Ok(Self {
            offset: 0,
            bytes,
            versions,
        })
    }
}

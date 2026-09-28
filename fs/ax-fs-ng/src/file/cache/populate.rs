//! Prepare pages and retire victims without retaining the cache index.

use alloc::vec::Vec;

use axfs_ng_vfs::{FileNode, VfsError, VfsResult};

use super::{CachedFile, PAGE_SIZE, PageCache, backing};

impl CachedFile {
    /// The caller owns `io_lock`, excluding insertion, resize and reclaim.
    /// Preparation cannot detach any existing mapped or dirty owner.
    pub(super) fn ensure_page_locked(
        &self,
        file: &FileNode,
        pn: u32,
        read_backing: bool,
    ) -> VfsResult<Option<(u32, PageCache)>> {
        if self.shared.page_cache.lock().contains(&pn) {
            return Ok(None);
        }
        let mut page = PageCache::new()?;
        if self.in_memory || !read_backing {
            page.data().fill(0);
        } else {
            let read = file.read_at(&mut page.data(), u64::from(pn) * PAGE_SIZE as u64)?;
            if read > PAGE_SIZE {
                return Err(VfsError::Io);
            }
            page.data()[read..].fill(0);
        }
        self.insert_prepared_page_locked(file, pn, page)
    }

    /// Publishes fully initialized backing under the caller's I/O exclusion.
    pub(super) fn insert_prepared_page_locked(
        &self,
        file: &FileNode,
        pn: u32,
        page: PageCache,
    ) -> VfsResult<Option<(u32, PageCache)>> {
        let attempts = {
            let cache = self.shared.page_cache.lock();
            if cache.contains(&pn) {
                return Ok(None);
            }
            if cache.len() >= cache.cap().get() {
                cache.len().min(16)
            } else {
                0
            }
        };
        let mut evicted = None;
        for _ in 0..attempts {
            let victim = self.shared.page_cache.lock().pop_lru();
            let Some((number, mut victim)) = victim else {
                break;
            };
            if victim.writeback_in_progress() {
                // An older owned write may still be in flight. Evicting a
                // redirtied version now could persist it before that old write.
                self.shared.page_cache.lock().put(number, victim);
                continue;
            }
            // io_lock reserves this detached slot. Cache-hit readers may see
            // absence but cannot fill it until this transaction completes.
            match self.evict_cache(file, number, &mut victim) {
                Ok(true) => {
                    evicted = Some((number, victim));
                    break;
                }
                Ok(false) => {
                    self.shared.page_cache.lock().put(number, victim);
                }
                Err(error) => {
                    self.shared.page_cache.lock().put(number, victim);
                    return Err(error);
                }
            }
        }
        self.shared.page_cache.lock().put(pn, page);
        Ok(evicted)
    }

    fn evict_cache(&self, file: &FileNode, pn: u32, page: &mut PageCache) -> VfsResult<bool> {
        let listeners = self
            .shared
            .evict_listeners
            .lock()
            .iter()
            .map(|entry| entry.listener.clone())
            .collect::<Vec<_>>();
        if !listeners.iter().all(|listener| listener(pn, page)) {
            return Ok(false);
        }
        if page.dirty {
            let page_start = u64::from(pn) * PAGE_SIZE as u64;
            let len = self
                .shared
                .len()
                .saturating_sub(page_start)
                .min(PAGE_SIZE as u64) as usize;
            if len > 0 {
                let mut snapshot = Vec::new();
                snapshot
                    .try_reserve_exact(len)
                    .map_err(|_| VfsError::NoMemory)?;
                snapshot.extend_from_slice(&page.data()[..len]);
                // The detached victim may still have COW source pins. Release
                // its byte guard before storage I/O; only this private snapshot
                // remains borrowed while the device can sleep.
                backing::write_all_at(file, &snapshot, page_start)?;
            }
            page.dirty = false;
        }
        Ok(true)
    }
}

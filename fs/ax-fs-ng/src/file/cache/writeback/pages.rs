//! Finite page reservations, retired on every success and failure path.

use super::*;

impl<'a> WritebackPages<'a> {
    pub(super) fn begin(
        shared: &'a CachedFileShared,
        requested: Option<&[u32]>,
    ) -> VfsResult<Self> {
        let requested = requested
            .map(|numbers| -> VfsResult<Vec<u32>> {
                let mut copy = Vec::new();
                copy.try_reserve_exact(numbers.len())
                    .map_err(|_| VfsError::NoMemory)?;
                copy.extend_from_slice(numbers);
                copy.sort_unstable();
                copy.dedup();
                Ok(copy)
            })
            .transpose()?;
        let mut round = Self {
            shared,
            file_len: 0,
            pages: Vec::new(),
        };
        let result = (|| -> VfsResult<()> {
            let _io = shared.io_lock.lock();
            let file_len = shared.len();
            round.file_len = file_len;
            let selected = |number: u32| {
                u64::from(number) * (PAGE_SIZE as u64) < file_len
                    && requested
                        .as_ref()
                        .is_none_or(|numbers| numbers.binary_search(&number).is_ok())
            };
            let count = shared
                .page_cache
                .lock()
                .iter()
                .filter(|(number, page)| page.dirty && selected(**number))
                .count();
            round
                .pages
                .try_reserve_exact(count)
                .map_err(|_| VfsError::NoMemory)?;
            let mut cache = shared.page_cache.lock();
            for (&number, page) in cache.iter_mut() {
                if page.dirty && selected(number) {
                    let paddr = page.paddr()?;
                    let pins = page.pins.checked_add(1).ok_or(VfsError::ValueOverflow)?;
                    page.begin_writeback()?;
                    page.pins = pins;
                    round.pages.push(WritebackPage {
                        number,
                        paddr,
                        protected: false,
                    });
                }
            }
            Ok(())
        })();
        // The closure's io/cache guards end before an error drops this owner.
        result?;
        round.pages.sort_unstable_by_key(|page| page.number);
        Ok(round)
    }

    pub(super) fn protect(&mut self) -> VfsResult<()> {
        for page in &mut self.pages {
            self.shared
                .protect_dirty_pages_before_writeback(core::slice::from_ref(&page.number))?;
            page.protected = true;
        }
        Ok(())
    }

    #[cfg(any(feature = "vfs", feature = "ext4"))]
    pub(super) fn protect_available(&mut self) -> VfsResult<()> {
        for page in &mut self.pages {
            match self
                .shared
                .protect_dirty_pages_before_writeback(core::slice::from_ref(&page.number))
            {
                Ok(()) => page.protected = true,
                // Background rounds leave contended pages dirty for the next scan.
                Err(VfsError::ResourceBusy) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub(super) fn numbers(&self) -> VfsResult<Vec<u32>> {
        let mut numbers = Vec::new();
        numbers
            .try_reserve_exact(self.pages.len())
            .map_err(|_| VfsError::NoMemory)?;
        numbers.extend(self.pages.iter().map(|page| page.number));
        Ok(numbers)
    }
}

impl Drop for WritebackPages<'_> {
    fn drop(&mut self) {
        let mut cache = self.shared.page_cache.lock();
        for tracked in &self.pages {
            if let Some(page) = cache.peek_mut(&tracked.number)
                && page.paddr() == Ok(tracked.paddr)
                && page.pins != 0
            {
                page.finish_writeback();
                page.pins -= 1;
            } else {
                warn!("writeback reservation lost cached page {}", tracked.number);
            }
        }
    }
}

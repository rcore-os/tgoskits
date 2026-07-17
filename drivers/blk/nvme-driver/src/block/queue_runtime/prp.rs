//! NVMe physical-region page construction and list ownership.

use alloc::vec::Vec;

use dma_api::{CoherentArray, PreparedDma};
use rdif_block::BlkError;

use crate::{err::Result as NvmeResult, nvme::Nvme};

const MAX_PRP_LIST_PAGES: usize = 1;

pub(super) struct PrpMapping {
    pub(super) prp1: u64,
    pub(super) prp2: u64,
    pub(super) prp_list: Option<CoherentArray<u64>>,
}

pub(super) fn build_prp_mapping(
    free_prp_lists: &mut Vec<CoherentArray<u64>>,
    page_size: usize,
    dma: &PreparedDma,
) -> Result<PrpMapping, BlkError> {
    let mut prps = PrpPageAccumulator::new();
    let segment = dma.segment();
    prps.push_segment(segment.addr.as_u64(), segment.len.get(), page_size)?;
    let pages = prps.into_pages();
    let prp1 = *pages.first().ok_or(BlkError::InvalidRequest)?;
    let prp2 = match pages.len() {
        1 => 0,
        2 => pages[1],
        _ => {
            let list_entries = page_size / core::mem::size_of::<u64>();
            if pages.len() - 1 > list_entries * MAX_PRP_LIST_PAGES {
                return Err(BlkError::InvalidRequest);
            }
            let mut list = free_prp_lists.pop().ok_or(BlkError::Retry)?;
            for entry in 0..list_entries {
                list.set_cpu(entry, 0);
            }
            for (entry, addr) in pages[1..].iter().copied().enumerate() {
                list.set_cpu(entry, addr);
            }
            let addr = list.dma_addr().as_u64();
            return Ok(PrpMapping {
                prp1,
                prp2: addr,
                prp_list: Some(list),
            });
        }
    };
    Ok(PrpMapping {
        prp1,
        prp2,
        prp_list: None,
    })
}

pub(in crate::block) fn alloc_prp_lists(
    nvme: &Nvme,
    depth: usize,
) -> NvmeResult<Vec<CoherentArray<u64>>> {
    let mut lists = Vec::with_capacity(depth);
    for _ in 0..depth {
        lists.push(nvme.alloc_prp_list()?);
    }
    Ok(lists)
}

#[derive(Default)]
pub(in crate::block) struct PrpPageAccumulator {
    pages: Vec<u64>,
    last_end: Option<u64>,
    current_page_end: Option<u64>,
}

impl PrpPageAccumulator {
    pub(in crate::block) const fn new() -> Self {
        Self {
            pages: Vec::new(),
            last_end: None,
            current_page_end: None,
        }
    }

    pub(in crate::block) fn into_pages(self) -> Vec<u64> {
        self.pages
    }

    pub(in crate::block) fn push_segment(
        &mut self,
        addr: u64,
        len: usize,
        page_size: usize,
    ) -> Result<(), BlkError> {
        if page_size == 0 || len == 0 {
            return Err(BlkError::InvalidRequest);
        }
        let page_size = u64::try_from(page_size).map_err(|_| BlkError::InvalidRequest)?;
        let end = addr
            .checked_add(u64::try_from(len).map_err(|_| BlkError::InvalidRequest)?)
            .ok_or(BlkError::InvalidRequest)?;
        let mut cursor = addr;

        while cursor < end {
            self.ensure_page_entry(cursor, page_size)?;
            let page_end = self.current_page_end.ok_or(BlkError::InvalidRequest)?;
            let chunk_end = page_end.min(end);
            if chunk_end <= cursor {
                return Err(BlkError::InvalidRequest);
            }
            cursor = chunk_end;
            self.last_end = Some(cursor);
        }

        Ok(())
    }

    fn ensure_page_entry(&mut self, cursor: u64, page_size: u64) -> Result<(), BlkError> {
        let Some(last_end) = self.last_end else {
            self.push_page(cursor, page_size)?;
            return Ok(());
        };
        let current_page_end = self.current_page_end.ok_or(BlkError::InvalidRequest)?;

        if cursor < last_end {
            return Err(BlkError::InvalidRequest);
        }
        if cursor == last_end && cursor < current_page_end {
            return Ok(());
        }
        if cursor != last_end && last_end != current_page_end {
            return Err(BlkError::InvalidRequest);
        }
        if !cursor.is_multiple_of(page_size) {
            return Err(BlkError::InvalidRequest);
        }
        self.push_page(cursor, page_size)
    }

    fn push_page(&mut self, addr: u64, page_size: u64) -> Result<(), BlkError> {
        let page_base = addr / page_size * page_size;
        let page_end = page_base
            .checked_add(page_size)
            .ok_or(BlkError::InvalidRequest)?;
        self.pages.push(addr);
        self.current_page_end = Some(page_end);
        Ok(())
    }
}

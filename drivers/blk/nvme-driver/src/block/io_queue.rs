use super::*;

const MAX_PRP_LIST_PAGES: usize = 1;

pub(super) struct NvmeBlockQueue {
    id: usize,
    name: &'static str,
    namespace: Namespace,
    dma_mask: u64,
    page_size: usize,
    max_transfer_bytes: Option<usize>,
    depth: usize,
    queue: NvmeQueue,
    state: NvmeQueueState,
}

struct NvmeQueueState {
    slots: Vec<RequestSlot>,
    free_cids: Vec<usize>,
    free_prp_lists: Vec<CoherentArray<u64>>,
    prp_pages: Vec<u64>,
}

struct RequestSlot {
    pending: bool,
    prp_list: Option<CoherentArray<u64>>,
    dma: Option<InFlightDma>,
}

struct PrpMapping {
    prp1: u64,
    prp2: u64,
    prp_list: Option<CoherentArray<u64>>,
}

impl NvmeBlockQueue {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        id: usize,
        depth: usize,
        name: &'static str,
        namespace: Namespace,
        dma_mask: u64,
        page_size: usize,
        max_transfer_bytes: Option<usize>,
        queue: NvmeQueue,
        prp_lists: Vec<CoherentArray<u64>>,
    ) -> Self {
        let mut slots = Vec::with_capacity(depth + 1);
        slots.resize_with(depth + 1, || RequestSlot {
            pending: false,
            prp_list: None,
            dma: None,
        });
        Self {
            id,
            name,
            namespace,
            dma_mask,
            page_size,
            max_transfer_bytes,
            depth,
            queue,
            state: NvmeQueueState {
                slots,
                free_cids: (1..=depth).rev().collect(),
                free_prp_lists: prp_lists,
                prp_pages: Vec::with_capacity(page_size / core::mem::size_of::<u64>() + 1),
            },
        }
    }

    fn queue_info(&self) -> QueueInfo {
        QueueInfo {
            id: self.id,
            device: device_info(self.name, self.namespace),
            limits: limits(
                self.dma_mask,
                self.page_size,
                self.max_transfer_bytes,
                self.namespace,
                self.depth,
            ),
        }
    }

    fn stage_next(&mut self, requests: &mut OwnedRequestBatch) -> Result<RequestId, BlkError> {
        let Some(mut request) = requests.pop_front() else {
            return Err(BlkError::InvalidRequest);
        };
        if let Err(error) = validate_owned_request(self.queue_info(), &request) {
            requests.push_front(request);
            return Err(error);
        }

        let Some(cid) = self.state.free_cids.pop() else {
            requests.push_front(request);
            return Err(BlkError::Retry);
        };
        let mapping = match self
            .state
            .build_command(self.namespace, self.page_size, cid, &request)
        {
            Ok(mapping) => mapping,
            Err(error) => {
                self.state.free_cids.push(cid);
                requests.push_front(request);
                return Err(error);
            }
        };

        let dma = request.data.take().map(|dma| {
            // SAFETY: the backing is installed in the command slot before the
            // batch commit transfers ownership to the controller.
            unsafe { dma.into_in_flight() }
        });
        let slot = &mut self.state.slots[cid];
        slot.pending = true;
        slot.prp_list = mapping.prp_list;
        slot.dma = dma;
        self.queue.stage_io_data(mapping.command);
        Ok(RequestId::new(cid))
    }
}

impl NvmeQueueState {
    fn complete_one(
        &mut self,
        queue_id: usize,
        completion: NvmeCompletion,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        let cid = usize::from(completion.command_id);
        let Some(slot) = self.slots.get_mut(cid) else {
            warn!(
                "nvme queue {} returned out-of-range command id {}",
                queue_id, cid
            );
            return Err(BlkError::Io);
        };
        if !slot.pending {
            warn!(
                "nvme queue {} returned completion for free command id {}",
                queue_id, cid
            );
            return Err(BlkError::Io);
        }

        let result = if completion.status.is_success() {
            Ok(())
        } else {
            warn!(
                "nvme queue {} request {} failed: status={:#x}, result={:#x}",
                queue_id, cid, completion.status.0, completion.result
            );
            Err(BlkError::Io)
        };
        let dma = slot.dma.take().map(|dma| {
            // SAFETY: consuming a CQ entry and advancing the CQ head is the
            // controller's terminal ownership handoff for this command id.
            unsafe { dma.complete_after_quiesce() }
        });
        if let Some(prp_list) = slot.prp_list.take() {
            self.free_prp_lists.push(prp_list);
        }
        slot.pending = false;
        self.free_cids.push(cid);
        sink.complete(CompletedRequest::new(RequestId::new(cid), result, dma));
        Ok(())
    }

    fn build_command(
        &mut self,
        namespace: Namespace,
        page_size: usize,
        cid: usize,
        request: &OwnedRequest,
    ) -> Result<BuiltCommand, BlkError> {
        let cid = u16::try_from(cid).map_err(|_| BlkError::InvalidRequest)?;
        match request.op {
            RequestOp::Read | RequestOp::Write => {
                let prp = self.build_prp_mapping(page_size, request)?;
                let command = match request.op {
                    RequestOp::Read => CommandSet::nvm_cmd_read_with_cid(
                        namespace.id,
                        prp.prp1,
                        prp.prp2,
                        request.lba,
                        request.block_count,
                        cid,
                    ),
                    RequestOp::Write => CommandSet::nvm_cmd_write_with_cid(
                        namespace.id,
                        prp.prp1,
                        prp.prp2,
                        request.lba,
                        request.block_count,
                        cid,
                    ),
                    _ => unreachable!(),
                };
                Ok(BuiltCommand {
                    command,
                    prp_list: prp.prp_list,
                })
            }
            RequestOp::Flush => Ok(BuiltCommand {
                command: CommandSet::nvm_cmd_flush_with_cid(namespace.id, cid),
                prp_list: None,
            }),
        }
    }

    fn build_prp_mapping(
        &mut self,
        page_size: usize,
        request: &OwnedRequest,
    ) -> Result<PrpMapping, BlkError> {
        let data = request.data.as_ref().ok_or(BlkError::InvalidRequest)?;
        let (prp_pages, free_prp_lists) = (&mut self.prp_pages, &mut self.free_prp_lists);
        let mut prps = PrpPageAccumulator::new(prp_pages);
        for segment in data.segments() {
            prps.push_segment(segment.addr.as_u64(), segment.len.get(), page_size)?;
        }
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
}

impl HardwareQueue for NvmeBlockQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        self.queue_info()
    }

    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult {
        let mut accepted = 0;
        let limit = self.queue_info().limits.max_submit_batch;
        while accepted < limit && !requests.is_empty() {
            match self.stage_next(requests) {
                Ok(id) => {
                    sink.accepted(id);
                    accepted += 1;
                }
                Err(BlkError::Retry) => {
                    return BatchSubmitResult::new(accepted, BatchSubmitDisposition::QueueFull);
                }
                Err(error) => {
                    return BatchSubmitResult::new(accepted, BatchSubmitDisposition::Fatal(error));
                }
            }
        }
        BatchSubmitResult::new(accepted, BatchSubmitDisposition::Continue)
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        self.queue.commit_io_submissions();
        Ok(())
    }

    fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        let mut drained = false;
        let mut first_error = None;
        while let Some(completion) = self.queue.take_completion_after_irq() {
            drained = true;
            if let Err(error) = self.state.complete_one(self.id, completion, sink) {
                first_error.get_or_insert(error);
            }
        }
        if drained {
            self.queue.commit_completion_head();
        }
        first_error.map_or(Ok(()), Err)
    }

    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        for cid in 1..self.state.slots.len() {
            let slot = &mut self.state.slots[cid];
            if !slot.pending {
                continue;
            }
            let dma = slot.dma.take().map(|dma| {
                // SAFETY: the runtime calls queue shutdown only after the
                // controller has confirmed CC.EN=0/CSTS.RDY=0.
                unsafe { dma.complete_after_quiesce() }
            });
            if let Some(prp_list) = slot.prp_list.take() {
                self.state.free_prp_lists.push(prp_list);
            }
            slot.pending = false;
            sink.complete(CompletedRequest::new(
                RequestId::new(cid),
                Err(BlkError::Io),
                dma,
            ));
        }
        self.state.free_cids = (1..=self.depth).rev().collect();
        Ok(())
    }
}

struct BuiltCommand {
    command: CommandSet,
    prp_list: Option<CoherentArray<u64>>,
}

pub(super) fn alloc_prp_lists(nvme: &Nvme, depth: usize) -> NvmeResult<Vec<CoherentArray<u64>>> {
    let mut lists = Vec::with_capacity(depth);
    for _ in 0..depth {
        lists.push(nvme.alloc_prp_list()?);
    }
    Ok(lists)
}

struct PrpPageAccumulator<'a> {
    pages: &'a mut Vec<u64>,
    last_end: Option<u64>,
    current_page_end: Option<u64>,
}

impl<'a> PrpPageAccumulator<'a> {
    fn new(pages: &'a mut Vec<u64>) -> Self {
        pages.clear();
        Self {
            pages,
            last_end: None,
            current_page_end: None,
        }
    }

    fn into_pages(self) -> &'a [u64] {
        self.pages.as_slice()
    }

    fn push_segment(&mut self, addr: u64, len: usize, page_size: usize) -> Result<(), BlkError> {
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

fn limits(
    dma_mask: u64,
    page_size: usize,
    controller_max_transfer_bytes: Option<usize>,
    namespace: Namespace,
    max_inflight: usize,
) -> QueueLimits {
    let lba_size = namespace.lba_size.max(1);
    let prp_entries = page_size / core::mem::size_of::<u64>();
    let prp_capacity_bytes = page_size.saturating_mul(prp_entries + 1);
    let max_bytes = controller_max_transfer_bytes
        .map_or(prp_capacity_bytes, |max_transfer| {
            prp_capacity_bytes.min(max_transfer)
        })
        .max(lba_size);
    let max_blocks = max_bytes
        .checked_div(lba_size)
        .unwrap_or(1)
        .max(1)
        .min(u16::MAX as usize + 1) as u32;
    let max_bytes = (max_blocks as usize).saturating_mul(lba_size);
    QueueLimits {
        dma_mask,
        dma_domain: dma_api::DmaDomainId::legacy_global(),
        dma_alignment: lba_size,
        dma_length_alignment: lba_size,
        segment_boundary: None,
        max_inflight: max_inflight.max(1),
        max_submit_batch: max_inflight.max(1),
        max_blocks_per_request: max_blocks,
        max_segments: 1,
        max_segment_size: max_bytes,
        supported_flags: RequestFlags::NONE,
        supports_flush: true,
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use dma_api::CoherentArray;
    use rdif_block::{BlkError, CompletedRequest, CompletionSink};

    use super::{NvmeQueueState, PrpPageAccumulator, RequestSlot, limits};
    use crate::{Namespace, queue::NvmeCompletion};

    #[derive(Default)]
    struct RecordingSink {
        completions: usize,
    }

    impl CompletionSink for RecordingSink {
        fn complete(&mut self, _request: CompletedRequest) {
            self.completions += 1;
        }
    }

    fn empty_queue_state(depth: usize) -> NvmeQueueState {
        let mut slots = Vec::with_capacity(depth + 1);
        slots.resize_with(depth + 1, || RequestSlot {
            pending: false,
            prp_list: None,
            dma: None,
        });
        NvmeQueueState {
            slots,
            free_cids: (1..=depth).rev().collect(),
            free_prp_lists: Vec::<CoherentArray<u64>>::new(),
            prp_pages: Vec::new(),
        }
    }

    #[test]
    fn invalid_completion_id_fails_the_queue_instead_of_being_silently_dropped() {
        let mut state = empty_queue_state(1);
        let mut sink = RecordingSink::default();

        let free_cid = NvmeCompletion {
            command_id: 1,
            ..NvmeCompletion::default()
        };
        assert_eq!(
            state.complete_one(0, free_cid, &mut sink),
            Err(BlkError::Io)
        );
        let out_of_range = NvmeCompletion {
            command_id: 2,
            ..NvmeCompletion::default()
        };
        assert_eq!(
            state.complete_one(0, out_of_range, &mut sink),
            Err(BlkError::Io)
        );
        assert_eq!(sink.completions, 0);
    }

    #[test]
    fn queue_limits_enforce_lba_alignment_and_controller_transfer_limit() {
        let namespace = Namespace {
            id: 1,
            lba_size: 512,
            lba_count: 1024,
            metadata_size: 0,
        };
        let limits = limits(u64::MAX, 4096, Some(512 * 1024), namespace, 8);

        assert_eq!(limits.dma_alignment, 512);
        assert_eq!(limits.dma_length_alignment, 512);
        assert_eq!(limits.max_blocks_per_request, 1024);
        assert_eq!(limits.max_segment_size, 512 * 1024);
        assert_eq!(limits.max_segments, 1);
        assert_eq!(limits.max_submit_batch, 8);
        assert!(limits.supports_flush);
    }

    #[test]
    fn prp_pages_split_at_controller_page_boundaries() {
        let mut scratch = Vec::new();
        let mut pages = PrpPageAccumulator::new(&mut scratch);

        pages.push_segment(0x1800, 4096, 4096).unwrap();

        assert_eq!(pages.into_pages(), [0x1800, 0x2000]);
    }

    #[test]
    fn prp_pages_coalesce_contiguous_split_segments() {
        let mut scratch = Vec::new();
        let mut pages = PrpPageAccumulator::new(&mut scratch);

        pages.push_segment(0x1000, 4096, 4096).unwrap();
        pages.push_segment(0x2000, 2048, 4096).unwrap();
        pages.push_segment(0x2800, 2048, 4096).unwrap();

        assert_eq!(pages.into_pages(), [0x1000, 0x2000]);
    }

    #[test]
    fn prp_pages_reject_unaligned_non_contiguous_segment() {
        let mut scratch = Vec::new();
        let mut pages = PrpPageAccumulator::new(&mut scratch);

        pages.push_segment(0x1000, 2048, 4096).unwrap();

        assert!(pages.push_segment(0x2800, 512, 4096).is_err());
    }
}

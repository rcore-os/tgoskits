use rdif_block::RequestId;

#[derive(Clone, Copy, Debug)]
pub(super) enum SplitDirection {
    Read,
    Write,
}

pub(super) struct SplitTransfer {
    pub(super) direction: SplitDirection,
    pub(super) public_id: RequestId,
    pub(super) next_card_block: u32,
    pub(super) block_addr_step: u32,
    pub(super) buffer_addr: usize,
    pub(super) next_offset: usize,
    pub(super) remaining_blocks: u32,
}

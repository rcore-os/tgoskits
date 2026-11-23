use core::{mem::offset_of, sync::atomic::Ordering};

use crate::{
    IVC_RING_CAPACITY, IVC_SLOT_SIZE,
    ivshmem::{IvshmemLocalChannel, IvshmemMessageHeaderLayout, IvshmemPeerChannel},
    ring::IvcSlotError,
};

pub(crate) struct IvshmemSlotProducer<'a> {
    local: IvshmemLocalChannel<'a>,
    peer: IvshmemPeerChannel<'a>,
    local_incarnation: u32,
    peer_incarnation: u32,
}

impl<'a> IvshmemSlotProducer<'a> {
    pub(crate) const fn new(
        local: IvshmemLocalChannel<'a>,
        peer: IvshmemPeerChannel<'a>,
        local_incarnation: u32,
        peer_incarnation: u32,
    ) -> Self {
        Self {
            local,
            peer,
            local_incarnation,
            peer_incarnation,
        }
    }

    pub(crate) fn available_slots(&self) -> usize {
        if self.validate_session().is_err() {
            return 0;
        }
        let tail = self.local.load(
            offset_of!(IvshmemMessageHeaderLayout, tx_tail),
            Ordering::Relaxed,
        );
        let head = self
            .peer
            .load_acquire(offset_of!(IvshmemMessageHeaderLayout, rx_head));
        IVC_RING_CAPACITY.saturating_sub(pending_slots(tail, head).unwrap_or(IVC_RING_CAPACITY))
    }

    pub(crate) fn try_push_slot(&mut self, slot: &[u8; IVC_SLOT_SIZE]) -> Result<(), IvcSlotError> {
        self.validate_session()?;
        let tail = self.local.load(
            offset_of!(IvshmemMessageHeaderLayout, tx_tail),
            Ordering::Relaxed,
        );
        let head = self
            .peer
            .load_acquire(offset_of!(IvshmemMessageHeaderLayout, rx_head));
        if pending_slots(tail, head)? >= IVC_RING_CAPACITY {
            return Err(IvcSlotError::Full);
        }

        let slot_index = tail as usize % IVC_RING_CAPACITY;
        self.local.write_slot(slot_index, slot);
        // Do not publish a frame after the peer has started another session.
        self.validate_session()?;
        self.local.store(
            offset_of!(IvshmemMessageHeaderLayout, tx_tail),
            tail.wrapping_add(1),
            Ordering::Release,
        );
        Ok(())
    }

    fn validate_session(&self) -> Result<(), IvcSlotError> {
        validate_session(
            self.local,
            self.peer,
            self.local_incarnation,
            self.peer_incarnation,
        )
    }
}

pub(crate) struct IvshmemSlotConsumer<'a> {
    local: IvshmemLocalChannel<'a>,
    peer: IvshmemPeerChannel<'a>,
    local_incarnation: u32,
    peer_incarnation: u32,
}

impl<'a> IvshmemSlotConsumer<'a> {
    pub(crate) const fn new(
        local: IvshmemLocalChannel<'a>,
        peer: IvshmemPeerChannel<'a>,
        local_incarnation: u32,
        peer_incarnation: u32,
    ) -> Self {
        Self {
            local,
            peer,
            local_incarnation,
            peer_incarnation,
        }
    }

    pub(crate) fn try_peek_slot(
        &mut self,
        output: &mut [u8; IVC_SLOT_SIZE],
    ) -> Result<bool, IvcSlotError> {
        self.try_peek_slot_at(0, output)
    }

    pub(crate) fn has_pending_slots(&self) -> bool {
        if self.validate_session().is_err() {
            return false;
        }
        let head = self.local.load(
            offset_of!(IvshmemMessageHeaderLayout, rx_head),
            Ordering::Relaxed,
        );
        let tail = self
            .peer
            .load_acquire(offset_of!(IvshmemMessageHeaderLayout, tx_tail));
        pending_slots(tail, head).is_ok_and(|pending| pending != 0)
    }

    pub(crate) fn try_peek_slot_at(
        &self,
        offset: usize,
        output: &mut [u8; IVC_SLOT_SIZE],
    ) -> Result<bool, IvcSlotError> {
        self.validate_session()?;
        let head = self.local.load(
            offset_of!(IvshmemMessageHeaderLayout, rx_head),
            Ordering::Relaxed,
        );
        let tail = self
            .peer
            .load_acquire(offset_of!(IvshmemMessageHeaderLayout, tx_tail));
        let pending = pending_slots(tail, head)?;
        if offset >= IVC_RING_CAPACITY || offset >= pending {
            return Ok(false);
        }

        let slot_index = head.wrapping_add(offset as u32) as usize % IVC_RING_CAPACITY;
        self.peer.read_slot(slot_index, output);
        // A restart can reset the peer header while a slot is copied. Reject
        // the observation before the message state machine accepts any bytes.
        self.validate_session()?;
        Ok(true)
    }

    pub(crate) fn pop_slot(&mut self) -> Result<(), IvcSlotError> {
        self.validate_session()?;
        let head = self.local.load(
            offset_of!(IvshmemMessageHeaderLayout, rx_head),
            Ordering::Relaxed,
        );
        let tail = self
            .peer
            .load_acquire(offset_of!(IvshmemMessageHeaderLayout, tx_tail));
        if pending_slots(tail, head)? == 0 {
            return Err(IvcSlotError::CursorOutOfRange {
                producer: tail,
                consumer: head,
            });
        }
        self.local.store(
            offset_of!(IvshmemMessageHeaderLayout, rx_head),
            head.wrapping_add(1),
            Ordering::Release,
        );
        // Reject a reset that became visible before the operation completed.
        // If it starts after this final check, the receive linearizes before
        // the reset and no frame from the new generation can reuse this slot
        // until a new port has reset the local RX head.
        self.validate_session()
    }

    fn validate_session(&self) -> Result<(), IvcSlotError> {
        validate_session(
            self.local,
            self.peer,
            self.local_incarnation,
            self.peer_incarnation,
        )
    }
}

fn validate_session(
    local: IvshmemLocalChannel<'_>,
    peer: IvshmemPeerChannel<'_>,
    local_incarnation: u32,
    peer_incarnation: u32,
) -> Result<(), IvcSlotError> {
    let local_header = local.read_session().ok_or(IvcSlotError::SessionLost)?;
    let peer_header = peer.read_session().ok_or(IvcSlotError::SessionLost)?;
    if local_header.incarnation != local_incarnation
        || peer_header.incarnation != peer_incarnation
        || local_header.peer_incarnation != peer_incarnation
        || peer_header.peer_incarnation != local_incarnation
        || !local_header.acknowledged()
        || !peer_header.acknowledged()
    {
        return Err(IvcSlotError::SessionLost);
    }
    Ok(())
}

fn pending_slots(producer: u32, consumer: u32) -> Result<usize, IvcSlotError> {
    let pending = producer.wrapping_sub(consumer);
    if pending > IVC_RING_CAPACITY as u32 {
        return Err(IvcSlotError::CursorOutOfRange { producer, consumer });
    }
    Ok(pending as usize)
}

//! Single-owner transmit progression.

use alloc::{collections::VecDeque, vec::Vec};

use crate::{
    TxToken,
    protocol::{TxConfirmation, ethernet_tx_frame},
};

pub(crate) const TX_CAPACITY: usize = 128;

pub(crate) struct PendingTx {
    pub token: TxToken,
    pub frame: Vec<u8>,
}

/// One queued packet encoded into the frame a transmit write carries.
pub(crate) struct WireFrame {
    /// Token whose buffer returns to the runtime once the packet completes.
    pub token: TxToken,
    /// Encoded frame: SDIO header, host descriptor, payload, and the block
    /// padding the single-frame write form ends with.
    pub bytes: Vec<u8>,
    /// Stream length inside `bytes`; a write carrying several frames replaces
    /// everything past it with the next frame.
    pub stream_len: usize,
}

pub(crate) struct TxState {
    queue: VecDeque<PendingTx>,
}

impl TxState {
    pub(crate) const fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    pub(crate) fn enqueue(&mut self, token: TxToken, frame: Vec<u8>) -> Result<(), Vec<u8>> {
        if self.queue.len() >= TX_CAPACITY {
            return Err(frame);
        }
        self.queue.push_back(PendingTx { token, frame });
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Encodes the oldest queued packet for the wire.
    ///
    /// `Err` returns the token of a packet the encoder refused, so the caller
    /// can report it complete instead of keeping its buffer.
    pub(crate) fn take_wire_frame(
        &mut self,
        interface_index: u8,
        station_index: u8,
        v3: bool,
    ) -> Option<Result<WireFrame, TxToken>> {
        let pending = self.queue.pop_front()?;
        Some(
            ethernet_tx_frame(
                &pending.frame,
                interface_index,
                station_index,
                v3,
                TxConfirmation::None,
            )
            .map(|(bytes, stream_len)| WireFrame {
                token: pending.token,
                bytes,
                stream_len,
            })
            .map_err(|_| pending.token),
        )
    }

    pub(crate) fn drain_tokens(&mut self) -> impl Iterator<Item = TxToken> + '_ {
        self.queue.drain(..).map(|pending| pending.token)
    }
}

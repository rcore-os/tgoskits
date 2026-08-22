use alloc::collections::VecDeque;

use ax_task::IrqNotify;

use crate::sync::SpinLock;

pub(super) const TX_FRAME_BYTES: usize = 256;
const TX_FRAME_CAPACITY: usize = 16;

#[derive(Clone, Copy)]
pub(super) struct TxFrame {
    len: u16,
    bytes: [u8; TX_FRAME_BYTES],
}

impl TxFrame {
    fn new(bytes: &[u8]) -> Self {
        let mut frame = Self {
            len: bytes.len() as u16,
            bytes: [0; TX_FRAME_BYTES],
        };
        frame.bytes[..bytes.len()].copy_from_slice(bytes);
        frame
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

struct TxQueueState {
    accepting: bool,
    frames: VecDeque<TxFrame>,
}

/// The only runtime TX queue. Lock acquisition is the cross-CPU ordering point.
pub(super) struct TxIngress {
    state: SpinLock<TxQueueState>,
}

impl TxIngress {
    pub(super) fn new() -> Self {
        Self {
            state: SpinLock::new(TxQueueState {
                accepting: false,
                frames: VecDeque::with_capacity(TX_FRAME_CAPACITY),
            }),
        }
    }

    pub(super) fn try_write(&self, bytes: &[u8], notify: &IrqNotify) -> usize {
        let accepted = submit_locked(&mut self.state.lock_irqsave(), bytes);
        if accepted > 0 {
            notify.notify();
        }
        accepted
    }

    pub(super) fn try_write_text(&self, bytes: &[u8], notify: &IrqNotify) -> usize {
        let accepted = submit_text_locked(&mut self.state.lock_irqsave(), bytes);
        if accepted > 0 {
            notify.notify();
        }
        accepted
    }

    pub(super) fn pop(&self) -> Option<TxFrame> {
        self.state.lock_irqsave().frames.pop_front()
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.state.lock_irqsave().frames.is_empty()
    }

    pub(super) fn start_accepting(&self) {
        let mut state = self.state.lock_irqsave();
        state.frames.clear();
        state.accepting = true;
    }

    pub(super) fn stop_and_discard(&self) {
        let mut state = self.state.lock_irqsave();
        state.accepting = false;
        state.frames.clear();
    }

    pub(super) fn discard_pending(&self) {
        self.state.lock_irqsave().frames.clear();
    }

    pub(super) fn write_room(&self) -> usize {
        let state = self.state.lock_irqsave();
        if !state.accepting {
            return 0;
        }
        (TX_FRAME_CAPACITY - state.frames.len()) * TX_FRAME_BYTES
    }
}

fn submit_locked(state: &mut TxQueueState, bytes: &[u8]) -> usize {
    if bytes.is_empty() || !state.accepting {
        return 0;
    }

    let mut accepted = 0;
    while accepted < bytes.len() && state.frames.len() < TX_FRAME_CAPACITY {
        let end = (accepted + TX_FRAME_BYTES).min(bytes.len());
        state.frames.push_back(TxFrame::new(&bytes[accepted..end]));
        accepted = end;
    }
    accepted
}

fn submit_text_locked(state: &mut TxQueueState, bytes: &[u8]) -> usize {
    if bytes.is_empty() || !state.accepting {
        return 0;
    }

    let mut accepted = 0;
    while accepted < bytes.len() && state.frames.len() < TX_FRAME_CAPACITY {
        let mut encoded = [0; TX_FRAME_BYTES];
        let mut encoded_len = 0;
        while accepted < bytes.len() {
            let byte = bytes[accepted];
            let required = if byte == b'\n' { 2 } else { 1 };
            if encoded_len + required > encoded.len() {
                break;
            }
            if byte == b'\n' {
                encoded[encoded_len] = b'\r';
                encoded_len += 1;
            }
            encoded[encoded_len] = byte;
            encoded_len += 1;
            accepted += 1;
        }
        state
            .frames
            .push_back(TxFrame::new(&encoded[..encoded_len]));
    }
    accepted
}

pub(super) struct TxFrameCursor {
    frame: TxFrame,
    offset: usize,
}

impl TxFrameCursor {
    pub(super) fn new(frame: TxFrame) -> Self {
        Self { frame, offset: 0 }
    }

    pub(super) fn remaining(&self) -> &[u8] {
        &self.frame.bytes()[self.offset..]
    }

    pub(super) fn advance(&mut self, count: usize) {
        self.offset += count;
    }

    pub(super) fn is_complete(&self) -> bool {
        self.offset == self.frame.bytes().len()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::sync::Arc;
    use std::{
        sync::{Barrier, Mutex},
        thread,
    };

    use super::*;

    #[test]
    fn text_log_submission_expands_line_feeds_to_crlf() {
        let mut state = TxQueueState {
            accepting: true,
            frames: VecDeque::new(),
        };

        assert_eq!(submit_text_locked(&mut state, b"first\nsecond\n"), 13);

        let output = state
            .frames
            .drain(..)
            .flat_map(|frame| frame.bytes().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(output, b"first\r\nsecond\r\n");
    }

    #[test]
    fn queue_order_is_the_lock_linearization_order() {
        let mut state = TxQueueState {
            accepting: true,
            frames: VecDeque::new(),
        };
        assert_eq!(submit_locked(&mut state, b"first"), 5);
        assert_eq!(submit_locked(&mut state, b"second"), 6);
        assert_eq!(state.frames.pop_front().unwrap().bytes(), b"first");
        assert_eq!(state.frames.pop_front().unwrap().bytes(), b"second");
    }

    #[test]
    fn queue_accepts_partial_input_at_its_fixed_capacity() {
        let mut state = TxQueueState {
            accepting: true,
            frames: VecDeque::new(),
        };
        let bytes = [0x55; TX_FRAME_BYTES * (TX_FRAME_CAPACITY + 1)];

        assert_eq!(
            submit_locked(&mut state, &bytes),
            TX_FRAME_BYTES * TX_FRAME_CAPACITY
        );
        assert_eq!(submit_locked(&mut state, b"x"), 0);
    }

    #[test]
    fn log_backlog_does_not_consume_tty_capacity() {
        let ingress = TxIngress::new();
        ingress.start_accepting();
        let mailbox = Arc::new(crate::serial::log_mailbox::LogMailbox::new(1));
        assert!(mailbox.claim(0));

        for _ in 0..crate::serial::log_mailbox::LOG_SLOTS_PER_CPU {
            assert!(
                mailbox
                    .try_publish(
                        0,
                        crate::serial::log_mailbox::LogRecordMeta::print(0, None),
                        format_args!("log backlog")
                    )
                    .published()
            );
        }
        assert_eq!(
            ingress.write_room(),
            TX_FRAME_BYTES * TX_FRAME_CAPACITY,
            "kernel logs must not consume the sleepable TTY queue"
        );
    }

    #[test]
    fn discard_pending_drops_queued_frames_without_stopping_ingress() {
        let ingress = TxIngress::new();
        ingress.start_accepting();
        assert_eq!(
            submit_locked(&mut ingress.state.lock_irqsave(), b"stale"),
            5
        );

        ingress.discard_pending();

        assert!(!ingress.has_pending());
        assert_eq!(ingress.write_room(), TX_FRAME_BYTES * TX_FRAME_CAPACITY);
        assert_eq!(
            submit_locked(&mut ingress.state.lock_irqsave(), b"fresh"),
            5
        );
        assert_eq!(ingress.pop().unwrap().bytes(), b"fresh");
    }

    #[test]
    fn concurrent_multi_frame_submissions_do_not_interleave() {
        let state = Arc::new(Mutex::new(TxQueueState {
            accepting: true,
            frames: VecDeque::new(),
        }));
        let start = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for byte in *b"ab" {
            let state = state.clone();
            let start = start.clone();
            threads.push(thread::spawn(move || {
                let bytes = [byte; TX_FRAME_BYTES + 1];
                start.wait();
                assert_eq!(
                    submit_locked(&mut state.lock().unwrap(), &bytes),
                    bytes.len()
                );
            }));
        }
        start.wait();
        for thread in threads {
            thread.join().unwrap();
        }

        let mut state = state.lock().unwrap();
        let labels = state
            .frames
            .drain(..)
            .map(|frame| frame.bytes()[0])
            .collect::<Vec<_>>();
        assert!(labels == *b"aabb" || labels == *b"bbaa");
    }
}

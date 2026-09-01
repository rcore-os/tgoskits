//! UVC video stream.

use alloc::{vec, vec::Vec};
use core::task::{Context, Poll};

use ax_media::videobuffer::{Vb2MemOps, Vb2Queue};
use crab_usb::{
    EndpointHandle,
    usb_if::{
        endpoint::{RequestId, TransferRequest},
        err::USBError,
    },
};

use crate::frame::FrameParser;

pub(crate) const ISO_BATCH: usize = 64;
pub(crate) const ISO_DEPTH: usize = 3;

/// Pending ISO batch.
#[derive(Clone)]
pub struct IsoPending {
    endpoint: EndpointHandle,
    request_id: RequestId,
}

impl IsoPending {
    pub fn new(endpoint: EndpointHandle, request_id: RequestId) -> Self {
        Self {
            endpoint,
            request_id,
        }
    }

    pub(crate) fn poll(&self, cx: &mut Context<'_>) -> Poll<Result<Vec<usize>, USBError>> {
        match self.endpoint.poll_request(self.request_id, cx) {
            Poll::Ready(Ok(completion)) => Poll::Ready(Ok(completion
                .iso_packets
                .iter()
                .map(|packet| packet.actual_length)
                .collect())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(USBError::TransferError(err))),
            Poll::Pending => Poll::Pending,
        }
    }

    pub(crate) fn cancel(&self) -> Result<(), USBError> {
        self.endpoint
            .cancel(self.request_id)
            .map_err(USBError::TransferError)
    }
}

/// Frame assembler.
pub(crate) struct FrameAssembler<'a, M: Vb2MemOps> {
    parser: &'a mut FrameParser,
    dest: &'a mut Option<ax_media::videobuffer::ActiveFrame>,
    expected: Option<usize>,
    queue: &'a Vb2Queue<M>,
}

impl<'a, M: Vb2MemOps> FrameAssembler<'a, M> {
    pub(crate) fn new(
        parser: &'a mut FrameParser,
        dest: &'a mut Option<ax_media::videobuffer::ActiveFrame>,
        expected: Option<usize>,
        queue: &'a Vb2Queue<M>,
    ) -> Self {
        Self {
            parser,
            dest,
            expected,
            queue,
        }
    }

    pub(crate) fn process_batch(&mut self, data: &[u8], actuals: &[usize], slot_len: usize) {
        self.ensure_dest();
        for (i, &actual) in actuals.iter().enumerate() {
            if actual < 2 {
                continue;
            }
            let pkt = &data[i * slot_len..i * slot_len + actual];
            self.process_one_packet(pkt);
        }
    }

    fn process_one_packet(&mut self, pkt: &[u8]) {
        let mut result = self.push_packet(pkt);
        loop {
            if let Some(bytes) = result.bytes {
                self.handle_frame_completion(bytes);
            }
            if !result.retry {
                break;
            }
            result = self.push_packet(pkt);
        }
    }

    fn handle_frame_completion(&mut self, bytes: usize) {
        if bytes == 0 {
            *self.dest = self.queue.acquire();
            return;
        }
        let valid = self.expected.is_none_or(|exp| bytes == exp);
        if !valid {
            log::warn!(
                "[UVC] drop truncated frame: got {} expected {:?}",
                bytes,
                self.expected
            );
            return;
        }
        self.complete_frame(bytes);
    }

    fn ensure_dest(&mut self) {
        if self.dest.is_none() {
            *self.dest = self.queue.acquire()
        }
    }

    fn push_packet(&mut self, pkt: &[u8]) -> crate::frame::PushResult {
        let out: &mut [u8] = match self.dest.as_mut() {
            Some(frame) => frame.as_mut_slice(),
            None => &mut [],
        };
        self.parser.push_packet(pkt, out)
    }

    fn complete_frame(&mut self, bytes: usize) {
        if let Some(frame) = self.dest.take() {
            let _ = self.queue.commit(frame, bytes as u32);
        }
        *self.dest = self.queue.acquire()
    }
}

/// ISO stream pipeline.
pub(crate) struct IsoStream {
    slot_len: usize,
    packet_lengths: Vec<usize>,
    slots: Vec<Slot>,
}

struct Slot {
    buffer: Vec<u8>,
    pending: Option<IsoPending>,
}

impl IsoStream {
    pub(crate) fn new(slot_len: usize, depth: usize) -> Self {
        let packet_lengths = vec![slot_len; ISO_BATCH];
        let slots = (0..depth)
            .map(|_| Slot {
                buffer: vec![0u8; slot_len * ISO_BATCH],
                pending: None,
            })
            .collect();
        Self {
            slot_len,
            packet_lengths,
            slots,
        }
    }

    fn try_submit_one(
        &mut self,
        handle: &dyn crate::UvcHandle,
        endpoint: u8,
    ) -> Result<bool, USBError> {
        let slot = match self.slots.iter_mut().find(|s| s.pending.is_none()) {
            Some(s) => s,
            None => return Ok(false),
        };
        match handle.submit_endpoint_transfer(
            endpoint,
            TransferRequest::iso_in(&mut slot.buffer, &self.packet_lengths),
        ) {
            Ok(pending) => {
                slot.pending = Some(pending);
                Ok(true)
            }
            Err(USBError::SlotLimitReached) => Ok(false),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn fill(
        &mut self,
        handle: &dyn crate::UvcHandle,
        endpoint: u8,
    ) -> Result<usize, USBError> {
        let mut n = 0;
        while self.try_submit_one(handle, endpoint)? {
            n += 1;
        }
        Ok(n)
    }

    pub(crate) fn poll_next<M: Vb2MemOps>(
        &mut self,
        cx: &mut Context<'_>,
        assembler: &mut FrameAssembler<'_, M>,
    ) -> Poll<Result<(), USBError>> {
        for slot in &mut self.slots {
            let Some(pending) = slot.pending.as_ref() else {
                continue;
            };
            match pending.poll(cx) {
                Poll::Ready(Ok(actuals)) => {
                    assembler.process_batch(&slot.buffer, &actuals, self.slot_len);
                    slot.pending = None;
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                Poll::Pending => {}
            }
        }
        Poll::Pending
    }

    pub(crate) fn cancel_all(&self) -> Result<(), USBError> {
        for slot in &self.slots {
            if let Some(pending) = &slot.pending {
                let _ = pending.cancel();
            }
        }
        Ok(())
    }

    pub(crate) fn in_flight(&self) -> usize {
        self.slots.iter().filter(|s| s.pending.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use std::sync::Mutex;

    use ax_media::videobuffer::{ActiveFrame, MemPlane, Vb2MemOps, Vb2Queue};

    use super::FrameAssembler;
    use crate::frame::{FrameParser, PayloadHeaderFlags};

    #[derive(Default)]
    struct TestAlloc {
        storage: Mutex<Vec<Vec<u8>>>,
    }

    impl Vb2MemOps for TestAlloc {
        fn alloc(&self, sizes: &[u32]) -> Result<Vec<MemPlane>, ax_media::V4l2Error> {
            use core::ptr::NonNull;
            let mut storage = self.storage.lock().unwrap();
            let mut planes = Vec::new();
            for &size in sizes {
                let buf = vec![0u8; size as usize];
                let ptr = NonNull::new(buf.as_ptr() as *mut u8).unwrap();
                let offset = (storage.len() * 4096) as usize;
                storage.push(buf);
                planes.push(MemPlane::new(ptr, offset, size));
            }
            Ok(planes)
        }

        fn release(&self, _planes: &[MemPlane]) {}

        fn mmap(&self, _plane: &MemPlane) -> Vec<usize> {
            Vec::new()
        }
    }

    const FID0: u8 = 0;
    const FID1: u8 = PayloadHeaderFlags::FID.bits();
    const EOF: u8 = PayloadHeaderFlags::EOF.bits();

    fn pkt(flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(2 + payload.len());
        v.push(2);
        v.push(flags);
        v.extend_from_slice(payload);
        v
    }

    fn build_batch(packets: &[Vec<u8>], slot_len: usize) -> (Vec<u8>, Vec<usize>) {
        let mut data = vec![0u8; slot_len * packets.len()];
        let mut actuals = Vec::with_capacity(packets.len());
        for (i, p) in packets.iter().enumerate() {
            let len = p.len();
            data[i * slot_len..i * slot_len + len].copy_from_slice(p);
            actuals.push(len);
        }
        (data, actuals)
    }

    fn queue_with_buffers(n: u32, size: u32) -> Vb2Queue<TestAlloc> {
        let q = Vb2Queue::new(TestAlloc::default(), 2, 8);
        q.reqbufs(n, &[size]).unwrap();
        for i in 0..n {
            q.qbuf(i).unwrap();
        }
        q.streamon().unwrap();
        q
    }

    fn dest_slice_of(q: &Vb2Queue<TestAlloc>, idx: u32) -> Vec<u8> {
        let vb = q.buffer_snapshot(idx).unwrap();
        let plane = vb.planes.first().unwrap();
        let ptr = plane.as_ptr() as *const u8;
        let len = plane.length as usize;
        let bytesused = vb.bytesused as usize;
        // SAFETY: `ptr` comes from `TestAlloc` and `bytesused <= length`.
        let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
        slice[..bytesused].to_vec()
    }

    #[test]
    fn valid_frame_completes_and_moves_to_next_buffer() {
        let q = queue_with_buffers(2, 128);
        let mut parser = FrameParser::new();
        let mut dest: Option<ActiveFrame> = None;
        let expected = None;

        let packets = vec![
            pkt(FID0, b"sync"),
            pkt(FID1, b"\xFF\xD8aa"),
            pkt(FID1 | EOF, b"bb\xFF\xD9"),
        ];
        let slot_len = 32;
        let (data, actuals) = build_batch(&packets, slot_len);

        let mut asm = FrameAssembler::new(&mut parser, &mut dest, expected, &q);
        asm.process_batch(&data, &actuals, slot_len);
        drop(asm);

        assert!(q.is_readable(), "valid frame should be done");
        let idx = q.dqbuf().unwrap();
        let out = dest_slice_of(&q, idx);
        assert_eq!(out, b"\xFF\xD8aabb\xFF\xD9");
        assert!(dest.is_some());
        assert_ne!(dest.unwrap().index(), idx);
    }

    #[test]
    fn truncated_frame_is_dropped() {
        let q = queue_with_buffers(2, 128);
        let mut parser = FrameParser::new();
        let mut dest: Option<ActiveFrame> = None;
        let expected = Some(6);

        let packets = vec![
            pkt(FID0, b"x"),
            pkt(FID1, b"\xFF\xD8bb"),
            pkt(FID1, b"cc"),
            pkt(FID1 | EOF, b"dd\xFF\xD9"),
        ];
        let slot_len = 32;
        let (data, actuals) = build_batch(&packets, slot_len);

        let mut asm = FrameAssembler::new(&mut parser, &mut dest, expected, &q);
        asm.process_batch(&data, &actuals, slot_len);
        drop(asm);

        assert!(!q.is_readable(), "truncated frame should be dropped");
        assert!(dest.is_some());
    }

    #[test]
    fn fid_toggle_retry_writes_to_new_buffer() {
        let q = queue_with_buffers(3, 128);
        let mut parser = FrameParser::new();
        let mut dest: Option<ActiveFrame> = None;

        let packets1 = vec![pkt(FID0, b"x"), pkt(FID1, b"\xFF\xD8f1\xFF\xD9")];
        let slot_len = 32;
        let mut asm = FrameAssembler::new(&mut parser, &mut dest, None, &q);
        let pkt_toggle = pkt(FID0, b"\xFF\xD8f2");
        let pkt_next = pkt(FID0 | EOF, b"tail\xFF\xD9");
        let mut all_packets = packets1.clone();
        all_packets.push(pkt_toggle.clone());
        let (data, actuals) = build_batch(&all_packets, slot_len);
        asm.process_batch(&data, &actuals, slot_len);
        drop(asm);
        assert!(q.is_readable());
        let idx0 = q.dqbuf().unwrap();
        let out0 = dest_slice_of(&q, idx0);
        assert_eq!(out0, b"\xFF\xD8f1\xFF\xD9");

        let mut asm2 = FrameAssembler::new(&mut parser, &mut dest, None, &q);
        let (data2, actuals2) = build_batch(&[pkt_next.clone()], slot_len);
        asm2.process_batch(&data2, &actuals2, slot_len);
        drop(asm2);
        assert!(q.is_readable());
        let idx1 = q.dqbuf().unwrap();
        let out1 = dest_slice_of(&q, idx1);
        assert_eq!(out1, b"\xFF\xD8f2tail\xFF\xD9");
    }
}

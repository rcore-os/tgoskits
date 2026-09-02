use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    fmt::{self, Write},
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use crate::structured_log::{RuntimeLogContext, write_structured_prefix};

pub(crate) const LOG_RECORD_BYTES: usize = 1024;
pub(super) const LOG_SLOTS_PER_CPU: usize = 64;

const NO_OWNER: usize = usize::MAX;
const STATE_BITS: u32 = 2;
const STATE_MASK: u64 = (1 << STATE_BITS) - 1;
const SEQUENCE_MASK: u64 = u64::MAX >> STATE_BITS;
const SEQUENCE_HALF_RANGE: u64 = SEQUENCE_MASK.div_ceil(2);
const TRUNCATED: u8 = 1 << 0;
const TASK_ID_VALID: u8 = 1 << 1;
const TRUNCATION_MARKER: &[u8] = "\u{1b}[m...[truncated]\r\n".as_bytes();

#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    Free    = 0,
    Writing = 1,
    Ready   = 2,
    Reading = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LogRecordKind {
    Print,
    Log,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LogRecordMeta {
    pub(super) timestamp_nanos: u64,
    pub(super) task_id: Option<u64>,
    pub(super) kind: LogRecordKind,
}

impl LogRecordMeta {
    pub(super) const fn print(timestamp_nanos: u64, task_id: Option<u64>) -> Self {
        Self {
            timestamp_nanos,
            task_id,
            kind: LogRecordKind::Print,
        }
    }

    pub(super) const fn log(timestamp_nanos: u64, task_id: Option<u64>) -> Self {
        Self {
            timestamp_nanos,
            task_id,
            kind: LogRecordKind::Log,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LogRecord {
    pub(super) cpu_id: u32,
    sequence: u64,
    pub(super) timestamp_nanos: u64,
    pub(super) task_id: u64,
    source_len: u32,
    accepted_source_len: u32,
    len: u16,
    flags: u8,
    pub(super) kind: LogRecordKind,
    bytes: [u8; LOG_RECORD_BYTES],
}

impl LogRecord {
    const fn empty() -> Self {
        Self {
            cpu_id: 0,
            sequence: 0,
            timestamp_nanos: 0,
            task_id: 0,
            source_len: 0,
            accepted_source_len: 0,
            len: 0,
            flags: 0,
            kind: LogRecordKind::Print,
            bytes: [0; LOG_RECORD_BYTES],
        }
    }

    pub(super) fn format(
        cpu_id: usize,
        sequence: u64,
        meta: LogRecordMeta,
        args: fmt::Arguments<'_>,
    ) -> Result<Self, fmt::Error> {
        let mut record = Self {
            cpu_id: cpu_id as u32,
            sequence,
            timestamp_nanos: meta.timestamp_nanos,
            task_id: meta.task_id.unwrap_or(0),
            source_len: 0,
            accepted_source_len: 0,
            len: 0,
            flags: if meta.task_id.is_some() {
                TASK_ID_VALID
            } else {
                0
            },
            kind: meta.kind,
            bytes: [0; LOG_RECORD_BYTES],
        };
        {
            let mut formatter = RecordFormatter {
                record: &mut record,
                truncated: false,
            };
            if meta.kind == LogRecordKind::Log {
                write_structured_prefix(
                    &mut formatter,
                    RuntimeLogContext::new(
                        core::time::Duration::from_nanos(meta.timestamp_nanos),
                        Some(cpu_id),
                        meta.task_id,
                    ),
                )?;
            }
            formatter.write_fmt(args)?;
            formatter.finish();
        }
        Ok(record)
    }

    pub(crate) fn cpu_id(&self) -> usize {
        self.cpu_id as usize
    }

    pub(super) fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) fn timestamp_nanos(&self) -> u64 {
        self.timestamp_nanos
    }

    pub(crate) fn task_id(&self) -> Option<u64> {
        (self.flags & TASK_ID_VALID != 0).then_some(self.task_id)
    }

    pub(crate) fn kind(&self) -> LogRecordKind {
        self.kind
    }

    pub(crate) fn is_truncated(&self) -> bool {
        self.flags & TRUNCATED != 0
    }

    pub(crate) fn source_len(&self) -> usize {
        self.source_len as usize
    }

    pub(super) fn accepted_source_len(&self) -> usize {
        self.accepted_source_len as usize
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

struct RecordFormatter<'a> {
    record: &'a mut LogRecord,
    truncated: bool,
}

impl RecordFormatter<'_> {
    fn finish(&mut self) {
        if !self.truncated {
            self.record.accepted_source_len = self.record.source_len;
            return;
        }

        let mut keep = (self.record.len as usize).min(LOG_RECORD_BYTES - TRUNCATION_MARKER.len());
        while keep > 0 && !core::str::from_utf8(&self.record.bytes[..keep]).is_ok() {
            keep -= 1;
        }
        if keep > 0
            && self.record.bytes[keep - 1] == b'\r'
            && self.record.bytes.get(keep) == Some(&b'\n')
        {
            keep -= 1;
        }
        let accepted = keep
            - self.record.bytes[..keep]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count();
        let end = keep + TRUNCATION_MARKER.len();
        self.record.bytes[keep..end].copy_from_slice(TRUNCATION_MARKER);
        self.record.len = end as u16;
        self.record.accepted_source_len = accepted.min(u32::MAX as usize) as u32;
        self.record.flags |= TRUNCATED;
    }
}

impl Write for RecordFormatter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.record.source_len = self
            .record
            .source_len
            .saturating_add(text.len().min(u32::MAX as usize) as u32);
        if self.truncated {
            return Ok(());
        }

        for character in text.chars() {
            let required = character.len_utf8() + usize::from(character == '\n');
            let encoded = self.record.len as usize;
            if encoded + required > LOG_RECORD_BYTES {
                self.truncated = true;
                break;
            }
            let mut next = encoded;
            if character == '\n' {
                self.record.bytes[next] = b'\r';
                next += 1;
            }
            let end = next + character.len_utf8();
            character.encode_utf8(&mut self.record.bytes[next..end]);
            self.record.len = end as u16;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PublishOutcome {
    #[cfg(test)]
    accepted_source_bytes: usize,
    dropped_source_bytes: usize,
    dropped_records: usize,
    published: bool,
    truncated: bool,
}

impl PublishOutcome {
    pub(super) fn dropped(source_len: usize) -> Self {
        Self {
            #[cfg(test)]
            accepted_source_bytes: 0,
            dropped_source_bytes: source_len,
            dropped_records: 1,
            published: false,
            truncated: false,
        }
    }

    #[cfg(test)]
    pub(super) fn accepted_source_bytes(self) -> usize {
        self.accepted_source_bytes
    }

    pub(super) fn dropped_source_bytes(self) -> usize {
        self.dropped_source_bytes
    }

    pub(super) fn dropped_records(self) -> usize {
        self.dropped_records
    }

    pub(super) fn published(self) -> bool {
        self.published
    }

    pub(super) fn truncated(self) -> bool {
        self.truncated
    }
}

#[repr(align(64))]
struct LogSlot {
    state: AtomicU64,
    record: UnsafeCell<LogRecord>,
}

impl LogSlot {
    const fn new() -> Self {
        Self {
            state: AtomicU64::new(pack_state(0, SlotState::Free)),
            record: UnsafeCell::new(LogRecord::empty()),
        }
    }
}

// SAFETY: the state machine grants one producer or the consumer exclusive
// ownership of `record`. Release publication of READY and Acquire acquisition
// of READY order every record access across coherent CPUs.
unsafe impl Sync for LogSlot {}

struct CpuLogRing {
    next_sequence: AtomicU64,
    producer_active: AtomicBool,
    slots: Box<[LogSlot]>,
}

impl CpuLogRing {
    fn new(slot_count: usize) -> Self {
        let mut slots = Vec::with_capacity(slot_count);
        slots.resize_with(slot_count, LogSlot::new);
        Self {
            next_sequence: AtomicU64::new(0),
            producer_active: AtomicBool::new(false),
            slots: slots.into_boxed_slice(),
        }
    }

    fn try_publish(
        &self,
        owner: &AtomicUsize,
        expected_owner: usize,
        cpu_id: usize,
        meta: LogRecordMeta,
        args: fmt::Arguments<'_>,
    ) -> PublishOutcome {
        let Some(_producer) = ProducerReservation::try_enter(&self.producer_active) else {
            return PublishOutcome::dropped(0);
        };
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed) & SEQUENCE_MASK;
        let slot = &self.slots[sequence as usize % self.slots.len()];
        let observed = slot.state.load(Ordering::Acquire);
        match unpack_state(observed) {
            SlotState::Reading | SlotState::Writing => return PublishOutcome::dropped(0),
            SlotState::Free | SlotState::Ready => {}
        }

        let writing = pack_state(sequence, SlotState::Writing);
        if slot
            .state
            .compare_exchange(observed, writing, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return PublishOutcome::dropped(0);
        }

        let reclaimed = unpack_state(observed) == SlotState::Ready;
        let reclaimed_bytes = if reclaimed {
            // SAFETY: the READY-to-WRITING CAS won exclusive ownership over
            // both the consumer and any lifecycle discard transition.
            unsafe { (&*slot.record.get()).source_len() }
        } else {
            0
        };
        let Ok(record) = LogRecord::format(cpu_id, sequence, meta, args) else {
            slot.state
                .store(pack_state(sequence, SlotState::Free), Ordering::Release);
            let mut outcome = PublishOutcome::dropped(reclaimed_bytes);
            outcome.dropped_records += usize::from(reclaimed);
            return outcome;
        };
        let accepted = record.accepted_source_len();
        let source_len = record.source_len();
        // SAFETY: WRITING ownership is exclusive until READY is published.
        unsafe { slot.record.get().write(record) };

        if owner.load(Ordering::Acquire) != expected_owner {
            slot.state
                .store(pack_state(sequence, SlotState::Free), Ordering::Release);
            let mut outcome = PublishOutcome::dropped(source_len + reclaimed_bytes);
            outcome.dropped_records += usize::from(reclaimed);
            return outcome;
        }
        slot.state
            .store(pack_state(sequence, SlotState::Ready), Ordering::Release);
        PublishOutcome {
            #[cfg(test)]
            accepted_source_bytes: accepted,
            dropped_source_bytes: source_len - accepted + reclaimed_bytes,
            dropped_records: usize::from(reclaimed),
            published: true,
            truncated: record.is_truncated(),
        }
    }

    fn take_oldest(&self) -> Option<LogRecord> {
        let (slot, observed) = self
            .slots
            .iter()
            .filter_map(|slot| {
                let observed = slot.state.load(Ordering::Acquire);
                (unpack_state(observed) == SlotState::Ready).then_some((slot, observed))
            })
            .min_by(|(_, left), (_, right)| {
                let left = unpack_sequence(*left);
                let right = unpack_sequence(*right);
                if sequence_before(left, right) {
                    core::cmp::Ordering::Less
                } else if left == right {
                    core::cmp::Ordering::Equal
                } else {
                    core::cmp::Ordering::Greater
                }
            })?;
        let sequence = unpack_sequence(observed);
        if slot
            .state
            .compare_exchange(
                observed,
                pack_state(sequence, SlotState::Reading),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return None;
        }
        // SAFETY: the READY-to-READING CAS excludes producer reclamation.
        let record = unsafe { *slot.record.get() };
        slot.state
            .store(pack_state(sequence, SlotState::Free), Ordering::Release);
        Some(record)
    }

    fn has_ready(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| unpack_state(slot.state.load(Ordering::Acquire)) == SlotState::Ready)
    }

    fn discard_ready(&self) {
        for slot in &self.slots {
            let mut observed = slot.state.load(Ordering::Acquire);
            while unpack_state(observed) == SlotState::Ready {
                match slot.state.compare_exchange_weak(
                    observed,
                    pack_state(unpack_sequence(observed), SlotState::Free),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(current) => observed = current,
                }
            }
        }
    }
}

struct ProducerReservation<'a> {
    active: &'a AtomicBool,
}

impl<'a> ProducerReservation<'a> {
    fn try_enter(active: &'a AtomicBool) -> Option<Self> {
        active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self { active })
    }
}

impl Drop for ProducerReservation<'_> {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

pub(super) struct LogMailbox {
    owner: AtomicUsize,
    rings: Box<[CpuLogRing]>,
    wake_ready: Box<[AtomicBool]>,
}

impl LogMailbox {
    pub(super) fn new(cpu_count: usize) -> Self {
        Self::with_slot_count(cpu_count, LOG_SLOTS_PER_CPU)
    }

    fn with_slot_count(cpu_count: usize, slot_count: usize) -> Self {
        assert!(cpu_count > 0);
        assert!(slot_count > 0);
        let mut rings = Vec::with_capacity(cpu_count);
        rings.resize_with(cpu_count, || CpuLogRing::new(slot_count));
        let mut wake_ready = Vec::with_capacity(cpu_count);
        wake_ready.resize_with(cpu_count, || AtomicBool::new(false));
        Self {
            owner: AtomicUsize::new(NO_OWNER),
            rings: rings.into_boxed_slice(),
            wake_ready: wake_ready.into_boxed_slice(),
        }
    }

    pub(super) fn mark_wake_ready(&self, cpu_id: usize) {
        self.wake_ready
            .get(cpu_id)
            .expect("log producer CPU must have a mailbox ring")
            .store(true, Ordering::Release);
    }

    pub(super) fn wake_ready(&self, cpu_id: usize) -> bool {
        self.wake_ready
            .get(cpu_id)
            .is_some_and(|ready| ready.load(Ordering::Acquire))
    }

    pub(super) fn claim(&self, runtime_index: usize) -> bool {
        self.discard_ready();
        self.owner
            .compare_exchange(NO_OWNER, runtime_index, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(super) fn release(&self, runtime_index: usize) {
        if self
            .owner
            .compare_exchange(runtime_index, NO_OWNER, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.discard_ready();
        }
    }

    pub(super) fn owned_by(&self, runtime_index: usize) -> bool {
        self.owner.load(Ordering::Acquire) == runtime_index
    }

    pub(super) fn try_publish(
        &self,
        cpu_id: usize,
        meta: LogRecordMeta,
        args: fmt::Arguments<'_>,
    ) -> PublishOutcome {
        let owner = self.owner.load(Ordering::Acquire);
        if owner == NO_OWNER {
            return PublishOutcome::dropped(0);
        }
        let Some(ring) = self.rings.get(cpu_id) else {
            return PublishOutcome::dropped(0);
        };
        ring.try_publish(&self.owner, owner, cpu_id, meta, args)
    }

    pub(super) fn has_pending_for(&self, runtime_index: usize) -> bool {
        self.owned_by(runtime_index) && self.rings.iter().any(CpuLogRing::has_ready)
    }

    fn discard_ready(&self) {
        for ring in &self.rings {
            ring.discard_ready();
        }
    }

    pub(super) fn reader(self: &Arc<Self>) -> LogReader {
        LogReader {
            mailbox: self.clone(),
            next_cpu: 0,
            expected_sequence: alloc::vec![None; self.rings.len()].into_boxed_slice(),
        }
    }
}

pub(super) struct LogReader {
    mailbox: Arc<LogMailbox>,
    next_cpu: usize,
    expected_sequence: Box<[Option<u64>]>,
}

impl LogReader {
    pub(super) fn take(&mut self, runtime_index: usize) -> Option<ConsumedLogRecord> {
        if !self.mailbox.owned_by(runtime_index) {
            return None;
        }
        for _ in 0..self.mailbox.rings.len() {
            let cpu_id = self.next_cpu;
            self.next_cpu = (self.next_cpu + 1) % self.mailbox.rings.len();
            let Some(record) = self.mailbox.rings[cpu_id].take_oldest() else {
                continue;
            };
            let expected = &mut self.expected_sequence[cpu_id];
            let sequence_gap = expected.map_or(0, |expected| {
                record.sequence().wrapping_sub(expected) & SEQUENCE_MASK
            });
            *expected = Some(record.sequence().wrapping_add(1) & SEQUENCE_MASK);
            return Some(ConsumedLogRecord {
                record,
                sequence_gap,
            });
        }
        None
    }

    #[cfg(test)]
    fn reset_round_robin(&mut self, next_cpu: usize) {
        self.next_cpu = next_cpu;
    }
}

pub(super) struct ConsumedLogRecord {
    pub(super) record: LogRecord,
    pub(super) sequence_gap: u64,
}

pub(super) struct LogRecordCursor {
    record: LogRecord,
    offset: usize,
}

impl LogRecordCursor {
    pub(super) fn new(record: LogRecord) -> Self {
        Self { record, offset: 0 }
    }

    pub(super) fn remaining(&self) -> &[u8] {
        &self.record.bytes()[self.offset..]
    }

    pub(super) fn advance(&mut self, count: usize) {
        self.offset += count;
    }

    pub(super) fn is_complete(&self) -> bool {
        self.offset == self.record.bytes().len()
    }
}

const fn pack_state(sequence: u64, state: SlotState) -> u64 {
    ((sequence & SEQUENCE_MASK) << STATE_BITS) | state as u64
}

const fn unpack_state(packed: u64) -> SlotState {
    match packed & STATE_MASK {
        0 => SlotState::Free,
        1 => SlotState::Writing,
        2 => SlotState::Ready,
        3 => SlotState::Reading,
        _ => unreachable!(),
    }
}

const fn unpack_sequence(packed: u64) -> u64 {
    packed >> STATE_BITS
}

const fn sequence_before(left: u64, right: u64) -> bool {
    let distance = left.wrapping_sub(right) & SEQUENCE_MASK;
    distance != 0 && distance >= SEQUENCE_HALF_RANGE
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{
        sync::{Arc as StdArc, Barrier},
        thread,
    };

    use super::*;

    const OWNER: usize = 7;

    fn meta() -> LogRecordMeta {
        LogRecordMeta::print(123, Some(456))
    }

    fn publish(
        mailbox: &LogMailbox,
        cpu_id: usize,
        meta: LogRecordMeta,
        text: &str,
    ) -> PublishOutcome {
        mailbox.try_publish(cpu_id, meta, format_args!("{text}"))
    }

    #[test]
    fn oldest_ready_slot_is_reclaimed_without_touching_tty_storage() {
        let mailbox = Arc::new(LogMailbox::with_slot_count(1, 2));
        assert!(mailbox.claim(OWNER));
        assert!(publish(&mailbox, 0, meta(), "first").published());
        assert!(publish(&mailbox, 0, meta(), "second").published());

        let outcome = publish(&mailbox, 0, meta(), "third");
        assert!(outcome.published());
        assert_eq!(outcome.dropped_source_bytes(), "first".len());

        let mut reader = mailbox.reader();
        assert_eq!(
            reader.take(OWNER).unwrap().record.bytes(),
            b"second".as_slice()
        );
        assert_eq!(
            reader.take(OWNER).unwrap().record.bytes(),
            b"third".as_slice()
        );
    }

    #[test]
    fn reading_or_writing_slot_causes_immediate_reservation_failure() {
        for busy in [SlotState::Reading, SlotState::Writing] {
            let mailbox = LogMailbox::with_slot_count(1, 1);
            assert!(mailbox.claim(OWNER));
            mailbox.rings[0].slots[0]
                .state
                .store(pack_state(0, busy), Ordering::Release);

            let outcome = publish(&mailbox, 0, meta(), "busy");
            assert!(!outcome.published());
            assert_eq!(outcome.dropped_source_bytes(), 0);
            assert_eq!(outcome.dropped_records(), 1);
        }
    }

    #[test]
    fn generation_wrap_preserves_per_cpu_fifo() {
        let mailbox = Arc::new(LogMailbox::with_slot_count(1, 4));
        assert!(mailbox.claim(OWNER));
        mailbox.rings[0]
            .next_sequence
            .store(SEQUENCE_MASK, Ordering::Relaxed);
        publish(&mailbox, 0, meta(), "last");
        publish(&mailbox, 0, meta(), "wrapped");

        let mut reader = mailbox.reader();
        let first = reader.take(OWNER).unwrap().record;
        let second = reader.take(OWNER).unwrap().record;
        assert_eq!(first.sequence(), SEQUENCE_MASK);
        assert_eq!(first.bytes(), b"last");
        assert_eq!(second.sequence(), 0);
        assert_eq!(second.bytes(), b"wrapped");
    }

    #[test]
    fn sequence_gap_reports_a_failed_reservation_exactly() {
        let mailbox = Arc::new(LogMailbox::with_slot_count(1, 2));
        assert!(mailbox.claim(OWNER));
        publish(&mailbox, 0, meta(), "zero");
        let mut reader = mailbox.reader();
        let first = reader.take(OWNER).unwrap();
        assert_eq!(first.sequence_gap, 0);

        mailbox.rings[0].slots[1]
            .state
            .store(pack_state(1, SlotState::Reading), Ordering::Release);
        assert!(!publish(&mailbox, 0, meta(), "lost").published());
        mailbox.rings[0].slots[1]
            .state
            .store(pack_state(1, SlotState::Free), Ordering::Release);
        publish(&mailbox, 0, meta(), "two");

        let second = reader.take(OWNER).unwrap();
        assert_eq!(second.record.sequence(), 2);
        assert_eq!(second.sequence_gap, 1);
    }

    #[test]
    fn reader_round_robins_cpus_while_preserving_local_fifo() {
        let mailbox = Arc::new(LogMailbox::with_slot_count(2, 4));
        assert!(mailbox.claim(OWNER));
        publish(&mailbox, 0, meta(), "0a");
        publish(&mailbox, 0, meta(), "0b");
        publish(&mailbox, 1, meta(), "1a");
        publish(&mailbox, 1, meta(), "1b");

        let mut reader = mailbox.reader();
        reader.reset_round_robin(0);
        let records = core::array::from_fn::<_, 4, _>(|_| {
            reader.take(OWNER).unwrap().record.bytes().to_vec()
        });
        assert_eq!(
            records,
            [
                b"0a".to_vec(),
                b"1a".to_vec(),
                b"0b".to_vec(),
                b"1b".to_vec()
            ]
        );
    }

    #[test]
    fn producer_and_consumer_transfer_complete_records_concurrently() {
        const RECORDS: usize = 32;

        let mailbox = Arc::new(LogMailbox::with_slot_count(1, LOG_SLOTS_PER_CPU));
        assert!(mailbox.claim(OWNER));
        let start = StdArc::new(Barrier::new(2));
        let producer = {
            let mailbox = mailbox.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                for sequence in 0..RECORDS {
                    let text = alloc::format!("record-{sequence:02}-checksum-{sequence:02}");
                    assert!(publish(&mailbox, 0, meta(), &text).published());
                }
            })
        };

        start.wait();
        let mut reader = mailbox.reader();
        for sequence in 0..RECORDS {
            let record = loop {
                if let Some(record) = reader.take(OWNER) {
                    break record;
                }
                thread::yield_now();
            };
            assert_eq!(record.sequence_gap, 0);
            assert_eq!(record.record.sequence(), sequence as u64);
            assert_eq!(
                record.record.bytes(),
                alloc::format!("record-{sequence:02}-checksum-{sequence:02}").as_bytes()
            );
        }
        producer.join().unwrap();
    }

    #[test]
    fn recursive_publish_is_dropped_without_disturbing_ready_data() {
        let mailbox = Arc::new(LogMailbox::with_slot_count(1, 2));
        assert!(mailbox.claim(OWNER));
        mailbox.rings[0]
            .producer_active
            .store(true, Ordering::Release);

        let outcome = publish(&mailbox, 0, meta(), "recursive");
        assert_eq!(outcome, PublishOutcome::dropped(0));
        assert_eq!(outcome.dropped_records(), 1);
        assert!(!mailbox.has_pending_for(OWNER));
    }

    #[test]
    fn utf8_truncation_keeps_a_boundary_and_exact_drop_count() {
        let mailbox = Arc::new(LogMailbox::with_slot_count(1, 1));
        assert!(mailbox.claim(OWNER));
        let text = "界".repeat(LOG_RECORD_BYTES);

        let outcome = publish(&mailbox, 0, meta(), &text);
        assert!(outcome.published());
        assert!(outcome.accepted_source_bytes() < text.len());
        assert_eq!(
            outcome.dropped_source_bytes(),
            text.len() - outcome.accepted_source_bytes()
        );
        let mut reader = mailbox.reader();
        let record = reader.take(OWNER).unwrap().record;
        assert!(record.is_truncated());
        assert!(core::str::from_utf8(record.bytes()).is_ok());
        assert!(record.bytes().ends_with(TRUNCATION_MARKER));
        assert_eq!(record.cpu_id(), 0);
        assert_eq!(record.timestamp_nanos(), 123);
        assert_eq!(record.task_id(), Some(456));
        assert_eq!(record.kind(), LogRecordKind::Print);
    }

    #[test]
    fn release_stops_publication_and_discards_ready_records() {
        let mailbox = Arc::new(LogMailbox::with_slot_count(1, 2));
        assert!(mailbox.claim(OWNER));
        publish(&mailbox, 0, meta(), "stale");

        mailbox.release(OWNER);

        assert!(!mailbox.has_pending_for(OWNER));
        assert_eq!(
            publish(&mailbox, 0, meta(), "closed"),
            PublishOutcome::dropped(0)
        );
    }

    #[test]
    fn structured_record_formats_runtime_metadata_once() {
        let mailbox = Arc::new(LogMailbox::with_slot_count(1, 1));
        assert!(mailbox.claim(OWNER));
        let meta = LogRecordMeta::log(12_345_678_901, Some(42));

        assert!(publish(&mailbox, 0, meta, "module:7] message\n").published());

        let mut reader = mailbox.reader();
        let record = reader.take(OWNER).unwrap().record;
        assert_eq!(
            record.bytes(),
            b"\x1b[37m[ 12.345678 0:42 module:7] message\r\n"
        );
        assert_eq!(record.kind(), LogRecordKind::Log);
    }
}

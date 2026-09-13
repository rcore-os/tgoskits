//! Shared publication order for complete logs and tagged console byte streams.

use alloc::collections::VecDeque;

use super::log_mailbox::{LOG_RECORD_BYTES, LogRecord, LogRecordKind};

pub(super) struct OrderedOutput {
    records: VecDeque<LogRecord>,
    capacity: usize,
}

impl OrderedOutput {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            records: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub(super) fn push(&mut self, record: LogRecord) -> Result<(), usize> {
        if self.records.len() == self.capacity {
            return Err(record.source_len());
        }
        self.records.push_back(record);
        Ok(())
    }

    // Caller holds the same IRQ-safe publication lock used for logs. Adjacent
    // writes may coalesce only when no intervening log or stream separates them.
    pub(super) fn write(&mut self, tag: u128, mut bytes: &[u8]) -> Result<(), usize> {
        let tail_room = self
            .records
            .back()
            .filter(|r| r.kind() == LogRecordKind::Output(tag))
            .map_or(0, |r| LOG_RECORD_BYTES - r.bytes().len());
        if bytes
            .len()
            .saturating_sub(tail_room)
            .div_ceil(LOG_RECORD_BYTES)
            > self.capacity - self.records.len()
        {
            return Err(bytes.len());
        }
        if tail_room != 0 {
            let accepted = self.records.back_mut().unwrap().append_output(bytes);
            bytes = &bytes[accepted..];
        }
        for chunk in bytes.chunks(LOG_RECORD_BYTES) {
            let mut record = LogRecord::output(tag);
            record.append_output(chunk);
            self.records.push_back(record);
        }
        Ok(())
    }

    pub(super) fn pop(&mut self) -> Option<LogRecord> {
        self.records.pop_front()
    }
    pub(super) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub(super) fn clear(&mut self) {
        self.records.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{super::log_mailbox::LogRecordMeta, *};

    fn log(text: &str) -> LogRecord {
        LogRecord::format(0, 0, LogRecordMeta::print(0, None), format_args!("{text}")).unwrap()
    }

    #[test]
    fn byte_writes_coalesce_without_overtaking_intervening_logs() {
        let mut queue = OrderedOutput::new(4);
        queue.push(log("before")).unwrap();
        queue.write(7, b"guest").unwrap();
        queue.write(7, b" line\n").unwrap();
        queue.push(log("middle")).unwrap();
        queue.write(7, b"next").unwrap();
        for expected in [b"before".as_slice(), b"guest line\n", b"middle", b"next"] {
            assert_eq!(queue.pop().unwrap().bytes(), expected);
        }
        assert!(queue.is_empty());
    }

    #[test]
    fn overflow_does_not_append_a_partial_write_or_block_later_output() {
        let mut queue = OrderedOutput::new(1);
        queue.write(1, b"old").unwrap();
        assert_eq!(
            queue.write(1, &[b'x'; LOG_RECORD_BYTES]),
            Err(LOG_RECORD_BYTES)
        );
        assert_eq!(queue.push(log("lost")), Err(4));
        assert_eq!(queue.pop().unwrap().bytes(), b"old");
        queue.write(2, b"new").unwrap();
        let record = queue.pop().unwrap();
        assert_eq!(record.kind(), LogRecordKind::Output(2));
        assert_eq!(record.bytes(), b"new");
    }
}

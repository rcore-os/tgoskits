//! Whole-record backlog for host logs hidden behind a foreground guest.

use alloc::{collections::VecDeque, format, vec::Vec};

const HOST_LOG_BACKLOG_CAPACITY: usize = 2 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct HostLogBacklog {
    records: VecDeque<Vec<u8>>,
    bytes: usize,
    dropped_records: usize,
    dropped_bytes: usize,
}

impl HostLogBacklog {
    pub fn push(&mut self, record: &[u8]) {
        if record.len() > HOST_LOG_BACKLOG_CAPACITY {
            self.dropped_records = self.dropped_records.saturating_add(1);
            self.dropped_bytes = self.dropped_bytes.saturating_add(record.len());
            return;
        }
        while self.bytes.saturating_add(record.len()) > HOST_LOG_BACKLOG_CAPACITY {
            let Some(dropped) = self.records.pop_front() else {
                break;
            };
            self.bytes -= dropped.len();
            self.dropped_records = self.dropped_records.saturating_add(1);
            self.dropped_bytes = self.dropped_bytes.saturating_add(dropped.len());
        }
        self.bytes += record.len();
        self.records.push_back(record.to_vec());
    }

    pub fn add_drops(&mut self, records: usize, bytes: usize) {
        self.dropped_records = self.dropped_records.saturating_add(records);
        self.dropped_bytes = self.dropped_bytes.saturating_add(bytes);
    }

    pub fn drain(&mut self) -> Vec<Vec<u8>> {
        let mut records = Vec::with_capacity(self.records.len().saturating_add(1));
        if self.dropped_records != 0 {
            records.push(
                format!(
                    "[Axvisor] dropped {} host log records ({} source bytes)\n",
                    self.dropped_records, self.dropped_bytes
                )
                .into_bytes(),
            );
        }
        records.extend(self.records.drain(..));
        self.bytes = 0;
        self.dropped_records = 0;
        self.dropped_bytes = 0;
        records
    }
}

#[cfg(any(test, axtest))]
mod tests {
    use alloc::vec;

    use super::*;

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn host_log_backlog_capacity_is_two_mib() {
        assert_eq!(HOST_LOG_BACKLOG_CAPACITY, 2 * 1024 * 1024);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn drops_oldest_complete_record_with_summary() {
        let mut backlog = HostLogBacklog::default();
        let record = vec![b'x'; HOST_LOG_BACKLOG_CAPACITY / 2];
        for _ in 0..3 {
            backlog.push(&record);
        }

        let replay = backlog.drain();
        assert_eq!(replay.len(), 3);
        assert!(replay[0].starts_with(b"[Axvisor] dropped 1 host log records"));
        assert!(replay[1..].iter().all(|retained| retained == &record));
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn oversized_record_does_not_evict_replayable_records() {
        let mut backlog = HostLogBacklog::default();
        let retained = vec![b'r'; 1024];
        backlog.push(&retained);
        backlog.push(&vec![b'x'; HOST_LOG_BACKLOG_CAPACITY + 1]);

        let replay = backlog.drain();
        assert_eq!(replay.len(), 2);
        assert!(replay[0].starts_with(b"[Axvisor] dropped 1 host log records"));
        assert_eq!(replay[1], retained);
    }
}

//! Bounded staging queue between guest UART exits and the physical console.

use alloc::{collections::VecDeque, format, vec::Vec};

pub(super) const HOST_OUTPUT_CAPACITY: usize = 64 * 1024;

#[derive(Debug, Default)]
pub(super) struct HostOutputQueue {
    pending: VecDeque<u8>,
    dropped_bytes: usize,
}

#[derive(Debug, Default)]
pub(super) struct HostOutputBatch {
    pending: VecDeque<u8>,
    dropped_bytes: usize,
}

impl HostOutputQueue {
    pub(super) fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        if bytes.len() >= HOST_OUTPUT_CAPACITY {
            self.dropped_bytes = self
                .dropped_bytes
                .saturating_add(self.pending.len())
                .saturating_add(bytes.len() - HOST_OUTPUT_CAPACITY);
            self.pending.clear();
            let retained = &bytes[bytes.len() - HOST_OUTPUT_CAPACITY..];
            self.retain_from_line_boundary(retained);
            return;
        }

        let required = self.pending.len().saturating_add(bytes.len());
        if required > HOST_OUTPUT_CAPACITY {
            self.drop_oldest(required - HOST_OUTPUT_CAPACITY);
            self.drop_partial_front_line();
        }
        self.pending.extend(bytes.iter().copied());
    }

    pub(super) fn take(&mut self) -> HostOutputBatch {
        HostOutputBatch {
            pending: core::mem::take(&mut self.pending),
            dropped_bytes: core::mem::take(&mut self.dropped_bytes),
        }
    }

    fn drop_oldest(&mut self, count: usize) {
        let dropped = count.min(self.pending.len());
        self.pending.drain(..dropped);
        self.dropped_bytes = self.dropped_bytes.saturating_add(dropped);
    }

    fn drop_partial_front_line(&mut self) {
        while let Some(byte) = self.pending.pop_front() {
            self.dropped_bytes = self.dropped_bytes.saturating_add(1);
            if byte == b'\n' {
                break;
            }
        }
    }

    fn retain_from_line_boundary(&mut self, retained: &[u8]) {
        let line_start = retained
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(0, |index| index + 1);
        self.dropped_bytes = self.dropped_bytes.saturating_add(line_start);
        self.pending.extend(retained[line_start..].iter().copied());
    }

    #[cfg(test)]
    pub(super) fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

impl HostOutputBatch {
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.dropped_bytes == 0
    }

    pub(super) fn into_bytes(mut self) -> Vec<u8> {
        if self.is_empty() {
            return Vec::new();
        }

        let marker = (self.dropped_bytes != 0).then(|| {
            format!(
                "\n[Axvisor] guest console output dropped {} bytes\n",
                self.dropped_bytes
            )
        });
        let marker_len = marker.as_ref().map_or(0, |marker| marker.len());
        let mut output = Vec::with_capacity(marker_len.saturating_add(self.pending.len()));
        if let Some(marker) = marker {
            output.extend_from_slice(marker.as_bytes());
        }
        output.extend(self.pending.drain(..));
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn output_queue_is_bounded_and_reports_overflow() {
        let mut queue = HostOutputQueue::default();
        queue.push(&vec![b'a'; HOST_OUTPUT_CAPACITY]);
        queue.push(b"latest line\n");

        assert!(queue.pending_len() <= HOST_OUTPUT_CAPACITY);
        let output = queue.take().into_bytes();
        assert!(
            output
                .windows(b"guest console output dropped".len())
                .any(|window| window == b"guest console output dropped")
        );
        assert!(output.ends_with(b"latest line\n"));
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn oversized_unterminated_write_retains_the_newest_bytes() {
        let mut queue = HostOutputQueue::default();
        queue.push(&vec![b'x'; HOST_OUTPUT_CAPACITY + 17]);

        let output = queue.take().into_bytes();

        assert!(output.ends_with(&vec![b'x'; HOST_OUTPUT_CAPACITY]));
        assert!(
            output
                .windows(b"guest console output dropped 17 bytes".len())
                .any(|window| window == b"guest console output dropped 17 bytes")
        );
    }
}

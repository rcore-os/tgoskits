//! Allocation-free Linux sample-id trailers shared by IRQ and sideband writers.

pub(super) const SAMPLE_ID_MAX_LEN: usize = 6 * 8;

/// Identity captured at record emission, in the event's PID namespace.
pub(super) struct SampleId {
    pub pid: u32,
    pub tid: u32,
    pub time: u64,
    pub id: u64,
    pub stream_id: u64,
    pub cpu: u32,
}

impl SampleId {
    /// Encodes only the selected trailer fields, in Linux's canonical order.
    pub fn encode(&self, sample_type: u64, buffer: &mut [u8; SAMPLE_ID_MAX_LEN]) -> usize {
        let fields = [
            (1 << 1, u64::from(self.pid) | (u64::from(self.tid) << 32)),
            (1 << 2, self.time),
            (1 << 6, self.id),
            (1 << 9, self.stream_id),
            (1 << 7, u64::from(self.cpu)),
            (1 << 16, self.id),
        ];
        let mut length = 0;
        for (bit, value) in fields {
            if sample_type & bit != 0 {
                buffer[length..length + 8].copy_from_slice(&value.to_ne_bytes());
                length += 8;
            }
        }
        length
    }
}

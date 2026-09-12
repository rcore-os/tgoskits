//! Allocation-free Linux sample-id trailers shared by IRQ and sideband writers.

pub(super) const SAMPLE_ID_MAX_LEN: usize = 6 * 8;
pub(super) const LOST_RECORD_MAX_LEN: usize = 24 + SAMPLE_ID_MAX_LEN;

/// Identity captured at record emission, in the event's PID namespace.
#[derive(Clone, Copy)]
pub(super) struct SampleId {
    pub pid: u32,
    pub tid: u32,
    pub time: u64,
    pub id: u64,
    pub stream_id: u64,
    pub cpu: u32,
}

impl SampleId {
    /// Encodes the LOST body and its optional Linux sample-id trailer.
    pub fn encode_lost(
        &self,
        lost: u64,
        sample_type: u64,
        sample_id_all: bool,
        buffer: &mut [u8; LOST_RECORD_MAX_LEN],
    ) -> usize {
        const PERF_RECORD_LOST: u32 = 2;
        let mut length = 24;
        if sample_id_all {
            let mut trailer = [0; SAMPLE_ID_MAX_LEN];
            // Linux __perf_output_begin redirects inherited output to the
            // parent before constructing LOST and its identity trailer.
            let parent = Self {
                stream_id: self.id,
                ..*self
            };
            let size = parent.encode(sample_type, &mut trailer);
            buffer[length..length + size].copy_from_slice(&trailer[..size]);
            length += size;
        }
        buffer[0..4].copy_from_slice(&PERF_RECORD_LOST.to_ne_bytes());
        buffer[4..6].copy_from_slice(&0u16.to_ne_bytes());
        buffer[6..8].copy_from_slice(&(length as u16).to_ne_bytes());
        buffer[8..16].copy_from_slice(&self.id.to_ne_bytes());
        buffer[16..24].copy_from_slice(&lost.to_ne_bytes());
        length
    }

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

#[cfg(all(test, axtest))]
mod tests {
    #[axtest::axtest]
    fn inherited_lost_body_and_trailer_name_the_output_parent() {
        let identity = super::SampleId {
            pid: 2,
            tid: 2,
            time: 9,
            id: 17,
            stream_id: 23,
            cpu: 0,
        };
        let mut record = [0; super::LOST_RECORD_MAX_LEN];
        let sample_type = (1 << 6) | (1 << 9) | (1 << 16);
        let length = identity.encode_lost(7, sample_type, true, &mut record);
        let words = record[..length]
            .chunks_exact(8)
            .map(|word| u64::from_ne_bytes(word.try_into().unwrap()))
            .collect::<alloc::vec::Vec<_>>();
        assert_eq!(length, 48);
        assert_eq!(
            &words[1..],
            &[17, 7, 17, 17, 17],
            "LOST body and trailer must name the redirected parent event"
        );
    }
}

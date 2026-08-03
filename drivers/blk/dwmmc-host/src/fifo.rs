//! FIFO depth and DMA burst programming.

const FIFOTH_MAX_WATERMARK: u16 = 0x0fff;
const DEFAULT_FIFO_DEPTH_WORDS: u16 = 0x100;
const DEFAULT_FIFOTH_MSIZE: u8 = 0x2;
const DMA_BURST_WORDS: [u16; 8] = [1, 4, 8, 16, 32, 64, 128, 256];

/// Native width of one controller FIFO entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FifoDataWidth {
    Bits16 = 2,
    Bits32 = 4,
    Bits64 = 8,
}

/// Immutable FIFO capability supplied by the SoC integration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FifoConfig {
    depth_words: u16,
    data_width: FifoDataWidth,
}

impl FifoConfig {
    /// Construct a FIFO capability when both watermarks fit `FIFOTH`.
    pub const fn new(depth_words: u16, data_width: FifoDataWidth) -> Option<Self> {
        if depth_words < 2 || depth_words / 2 > FIFOTH_MAX_WATERMARK {
            return None;
        }
        Some(Self {
            depth_words,
            data_width,
        })
    }

    pub const fn depth_words(self) -> u16 {
        self.depth_words
    }

    pub const fn data_width(self) -> FifoDataWidth {
        self.data_width
    }

    pub(crate) const fn baseline_threshold(self) -> u32 {
        let tx_wmark = self.depth_words / 2;
        let rx_wmark = tx_wmark.saturating_sub(1);
        encode_fifoth(DEFAULT_FIFOTH_MSIZE, rx_wmark, tx_wmark)
    }

    /// Match Linux `dw_mci_adjust_fifoth()` for an IDMAC data phase.
    pub(crate) fn dma_threshold(self, block_size: u32) -> u32 {
        let fifo_width = self.data_width as u32;
        let tx_wmark = self.depth_words / 2;
        let tx_wmark_inverse = self.depth_words - tx_wmark;
        let mut msize = 0_u8;
        let mut rx_wmark = 1_u16.min(self.depth_words.saturating_sub(1));

        if block_size.is_multiple_of(fifo_width) {
            let block_depth = block_size / fifo_width;
            for index in (1..DMA_BURST_WORDS.len()).rev() {
                let burst_words = DMA_BURST_WORDS[index];
                if block_depth.is_multiple_of(u32::from(burst_words))
                    && tx_wmark_inverse.is_multiple_of(burst_words)
                {
                    msize = index as u8;
                    rx_wmark = burst_words - 1;
                    break;
                }
            }
        }

        encode_fifoth(msize, rx_wmark, tx_wmark)
    }
}

impl Default for FifoConfig {
    fn default() -> Self {
        Self {
            depth_words: DEFAULT_FIFO_DEPTH_WORDS,
            data_width: FifoDataWidth::Bits32,
        }
    }
}

const fn encode_fifoth(msize: u8, rx_wmark: u16, tx_wmark: u16) -> u32 {
    ((msize as u32 & 0x7) << 28)
        | ((rx_wmark as u32 & FIFOTH_MAX_WATERMARK as u32) << 16)
        | (tx_wmark as u32 & FIFOTH_MAX_WATERMARK as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jh7110_dma_threshold_matches_linux_for_its_32_word_fifo() {
        let config = FifoConfig::new(32, FifoDataWidth::Bits32).unwrap();

        assert_eq!(config.baseline_threshold(), 0x200f_0010);
        assert_eq!(config.dma_threshold(512), 0x300f_0010);
    }

    #[test]
    fn default_profile_matches_the_legacy_reset_baseline() {
        assert_eq!(FifoConfig::default().baseline_threshold(), 0x207f_0080);
    }

    #[test]
    fn invalid_fifo_depth_is_rejected() {
        assert_eq!(FifoConfig::new(0, FifoDataWidth::Bits32), None);
        assert_eq!(FifoConfig::new(1, FifoDataWidth::Bits32), None);
    }
}

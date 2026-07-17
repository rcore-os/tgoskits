use rdif_block::DeviceInfo;

const IDENTIFY_BYTES: usize = 512;
const WORD_CAPABILITIES: usize = 49;
const WORD_LBA28_CAPACITY: usize = 60;
const WORD_COMMAND_SET_1: usize = 82;
const WORD_COMMAND_SET_2: usize = 83;
const WORD_SECTOR_SIZE: usize = 106;
const WORD_LBA48_CAPACITY: usize = 100;
const WORD_LOGICAL_SECTOR_WORDS: usize = 117;

const LBA_SUPPORTED: u16 = 1 << 9;
const LBA48_SUPPORTED: u16 = 1 << 10;
const FLUSH_SUPPORTED: u16 = 1 << 12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AtaDevice {
    pub num_blocks: u64,
    pub logical_block_size: usize,
    pub lba48: bool,
    pub flush: bool,
}

impl AtaDevice {
    pub(crate) fn parse_identify(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < IDENTIFY_BYTES {
            return None;
        }
        let word = |index: usize| {
            let offset = index * 2;
            u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
        };
        if word(WORD_CAPABILITIES) & LBA_SUPPORTED == 0 {
            return None;
        }

        let lba48 = word(WORD_COMMAND_SET_2) & LBA48_SUPPORTED != 0;
        let num_blocks = if lba48 {
            u64::from(word(WORD_LBA48_CAPACITY))
                | (u64::from(word(WORD_LBA48_CAPACITY + 1)) << 16)
                | (u64::from(word(WORD_LBA48_CAPACITY + 2)) << 32)
                | (u64::from(word(WORD_LBA48_CAPACITY + 3)) << 48)
        } else {
            u64::from(word(WORD_LBA28_CAPACITY)) | (u64::from(word(WORD_LBA28_CAPACITY + 1)) << 16)
        };
        if num_blocks == 0 {
            return None;
        }

        let sector_size_word = word(WORD_SECTOR_SIZE);
        let logical_block_size = if sector_size_word & (1 << 14) != 0
            && sector_size_word & (1 << 15) == 0
            && sector_size_word & (1 << 12) != 0
        {
            let words = u32::from(word(WORD_LOGICAL_SECTOR_WORDS))
                | (u32::from(word(WORD_LOGICAL_SECTOR_WORDS + 1)) << 16);
            usize::try_from(words).ok()?.checked_mul(2)?
        } else {
            512
        };
        if logical_block_size < 512 || !logical_block_size.is_power_of_two() {
            return None;
        }

        Some(Self {
            num_blocks,
            logical_block_size,
            lba48,
            flush: word(WORD_COMMAND_SET_1) & FLUSH_SUPPORTED != 0,
        })
    }

    pub(crate) fn device_info(self, name: &'static str) -> DeviceInfo {
        DeviceInfo {
            name: Some(name),
            vendor: Some("ATA"),
            ..DeviceInfo::new(self.num_blocks, self.logical_block_size)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lba48_capacity_and_logical_sector_size() {
        let mut identify = [0_u8; IDENTIFY_BYTES];
        set_word(&mut identify, WORD_CAPABILITIES, LBA_SUPPORTED);
        set_word(
            &mut identify,
            WORD_COMMAND_SET_2,
            LBA48_SUPPORTED | FLUSH_SUPPORTED,
        );
        set_word(&mut identify, WORD_COMMAND_SET_1, FLUSH_SUPPORTED);
        set_word(&mut identify, WORD_LBA48_CAPACITY, 0x3456);
        set_word(&mut identify, WORD_LBA48_CAPACITY + 1, 0x12);
        set_word(&mut identify, WORD_SECTOR_SIZE, (1 << 14) | (1 << 12));
        set_word(&mut identify, WORD_LOGICAL_SECTOR_WORDS, 2048);

        let device = AtaDevice::parse_identify(&identify).unwrap();

        assert_eq!(device.num_blocks, 0x12_3456);
        assert_eq!(device.logical_block_size, 4096);
        assert!(device.lba48);
        assert!(device.flush);
    }

    fn set_word(bytes: &mut [u8], index: usize, value: u16) {
        bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
    }
}

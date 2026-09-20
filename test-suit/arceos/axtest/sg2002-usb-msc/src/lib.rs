#![no_std]

const DEFAULT_READ_BENCH_SIZES: [usize; 5] = [512, 4096, 16 * 1024, 64 * 1024, 256 * 1024];
const MIN_READ_BENCH_BYTES: usize = 4 * 1024 * 1024;
const MIN_READ_BENCH_ITERS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BenchConfig {
    pub read_sizes: [usize; 5],
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            read_sizes: DEFAULT_READ_BENCH_SIZES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteBenchConfig {
    pub start_lba: u32,
    pub blocks: u16,
}

pub fn build_read10_command(lba: u32, blocks: u16) -> [u8; 10] {
    build_rw10_command(0x28, lba, blocks)
}

pub fn build_write10_command(lba: u32, blocks: u16) -> [u8; 10] {
    build_rw10_command(0x2a, lba, blocks)
}

fn build_rw10_command(opcode: u8, lba: u32, blocks: u16) -> [u8; 10] {
    let lba = lba.to_be_bytes();
    let blocks = blocks.to_be_bytes();
    [
        opcode, 0, lba[0], lba[1], lba[2], lba[3], 0, blocks[0], blocks[1], 0,
    ]
}

pub fn blocks_per_transfer(bytes: usize, block_size: usize) -> u16 {
    if bytes == 0 || block_size == 0 {
        return 0;
    }
    let blocks = bytes.div_ceil(block_size);
    blocks.min(u16::MAX as usize) as u16
}

pub fn bench_iterations(transfer_bytes: usize) -> usize {
    if transfer_bytes == 0 {
        return 0;
    }
    let min_bytes_iters = MIN_READ_BENCH_BYTES.div_ceil(transfer_bytes);
    min_bytes_iters.max(MIN_READ_BENCH_ITERS)
}

pub fn parse_write_bench_config(
    get: impl Fn(&str) -> Option<&'static str>,
) -> Option<WriteBenchConfig> {
    if get("SG2002_DWC2_WRITE_BENCH") != Some("1") {
        return None;
    }
    let start_lba = parse_u32_env(get("SG2002_DWC2_WRITE_LBA")?)?;
    let blocks = parse_u16_env(get("SG2002_DWC2_WRITE_BLOCKS")?)?;
    if blocks == 0 {
        return None;
    }
    Some(WriteBenchConfig { start_lba, blocks })
}

#[doc(hidden)]
pub fn compile_time_write_bench_config() -> Option<WriteBenchConfig> {
    parse_write_bench_config(|name| match name {
        "SG2002_DWC2_WRITE_BENCH" => option_env!("SG2002_DWC2_WRITE_BENCH"),
        "SG2002_DWC2_WRITE_LBA" => option_env!("SG2002_DWC2_WRITE_LBA"),
        "SG2002_DWC2_WRITE_BLOCKS" => option_env!("SG2002_DWC2_WRITE_BLOCKS"),
        _ => None,
    })
}

fn parse_u32_env(value: &str) -> Option<u32> {
    parse_u64_env(value).and_then(|value| u32::try_from(value).ok())
}

fn parse_u16_env(value: &str) -> Option<u16> {
    parse_u64_env(value).and_then(|value| u16::try_from(value).ok())
}

fn parse_u64_env(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        trimmed.parse().ok()
    }
}

#[doc(hidden)]
pub fn mib_per_sec_x100(bytes: usize, nanos: u64) -> u64 {
    if nanos == 0 {
        return 0;
    }
    let value = (bytes as u128) * 100 * 1_000_000_000u128 / (nanos as u128) / 1_048_576u128;
    value.min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod host_unit_tests {
    use super::{
        BenchConfig, WriteBenchConfig, bench_iterations, blocks_per_transfer, build_read10_command,
        build_write10_command, compile_time_write_bench_config, mib_per_sec_x100,
        parse_write_bench_config,
    };

    #[test]
    fn read_and_write10_commands_encode_lba_and_block_count() {
        assert_eq!(
            build_read10_command(0x0102_0304, 0x0020),
            [0x28, 0, 0x01, 0x02, 0x03, 0x04, 0, 0x00, 0x20, 0]
        );
        assert_eq!(
            build_write10_command(0x0a0b_0c0d, 0x0100),
            [0x2a, 0, 0x0a, 0x0b, 0x0c, 0x0d, 0, 0x01, 0x00, 0]
        );
    }

    #[test]
    fn bench_sizes_map_to_whole_blocks_and_minimum_iterations() {
        assert_eq!(
            BenchConfig::default().read_sizes,
            [512, 4096, 16 * 1024, 64 * 1024, 256 * 1024]
        );
        assert_eq!(blocks_per_transfer(4096, 512), 8);
        assert_eq!(blocks_per_transfer(4097, 512), 9);
        assert_eq!(bench_iterations(512), 8192);
        assert_eq!(bench_iterations(1024 * 1024), 8);
        assert_eq!(mib_per_sec_x100(1024 * 1024, 1_000_000_000), 100);
    }

    #[test]
    fn write_bench_requires_opt_in_lba_and_block_count() {
        assert_eq!(compile_time_write_bench_config(), None);
        assert_eq!(parse_write_bench_config(|_| None), None);
        assert_eq!(
            parse_write_bench_config(|name| match name {
                "SG2002_DWC2_WRITE_BENCH" => Some("1"),
                "SG2002_DWC2_WRITE_LBA" => Some("4096"),
                "SG2002_DWC2_WRITE_BLOCKS" => Some("128"),
                _ => None,
            }),
            Some(WriteBenchConfig {
                start_lba: 4096,
                blocks: 128,
            })
        );
        assert_eq!(
            parse_write_bench_config(|name| match name {
                "SG2002_DWC2_WRITE_BENCH" => Some("1"),
                "SG2002_DWC2_WRITE_LBA" => Some("4096"),
                _ => None,
            }),
            None
        );
    }
}

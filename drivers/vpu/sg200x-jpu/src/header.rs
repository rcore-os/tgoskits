//! JPEG 头解析（SOF / DHT / DQT / SOS）。

use super::regs::{FORMAT_224, FORMAT_400, FORMAT_420, FORMAT_422, FORMAT_444};

pub struct JpegHeaderInfo {
    pub width: u32,
    pub height: u32,
    pub num_components: u32,
    pub format: u32,
    pub ecs_offset: usize,
    pub restart_interval: u32,
    pub dc_huff_tbl: [usize; 3],
    pub ac_huff_tbl: [usize; 3],
    pub quant_tbl: [usize; 3],
    pub huff_tables: [HuffTable; 4],
    pub quant_tables: [QuantTable; 4],
    pub huff_table_count: usize,
    pub quant_table_count: usize,
}

pub struct HuffTable {
    pub bits: [u8; 16],
    pub values: [u8; 256],
    pub num_values: usize,
    pub min_codes: [u32; 16],
    pub max_codes: [u32; 16],
    pub ptrs: [u8; 16],
}

impl HuffTable {
    pub fn new() -> Self {
        Self {
            bits: [0; 16],
            values: [0; 256],
            num_values: 0,
            min_codes: [0xFFFF; 16],
            max_codes: [0xFFFF; 16],
            ptrs: [0xFF; 16],
        }
    }

    pub fn sign_extend_16(huff_data: u32) -> u32 {
        if huff_data & 0x8000 != 0 { 0xFFFF } else { 0 }
    }

    pub fn sign_extend_8(huff_data: u32) -> u32 {
        if huff_data & 0x80 != 0 { 0xFFFFFF } else { 0 }
    }

    pub fn generate(&mut self) {
        let mut ptr_cnt: usize = 0;
        let mut huff_code: u32 = 0;
        let mut data_flag = false;

        for i in 0..16 {
            if self.bits[i] != 0 {
                self.ptrs[i] = ptr_cnt as u8;
                ptr_cnt += self.bits[i] as usize;
                self.min_codes[i] = huff_code;
                self.max_codes[i] = huff_code + (self.bits[i] as u32 - 1);
                data_flag = true;
            } else {
                self.ptrs[i] = 0xFF;
                self.min_codes[i] = 0xFFFF;
                self.max_codes[i] = 0xFFFF;
            }

            if data_flag {
                if self.bits[i] == 0 {
                    huff_code <<= 1;
                } else {
                    huff_code = (self.max_codes[i] + 1) << 1;
                }
            }
        }
    }
}

pub struct QuantTable {
    pub values: [u16; 64],
}

impl QuantTable {
    pub fn new() -> Self {
        Self { values: [0; 64] }
    }
}

impl JpegHeaderInfo {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            num_components: 0,
            format: FORMAT_420,
            ecs_offset: 0,
            restart_interval: 0,
            dc_huff_tbl: [0; 3],
            ac_huff_tbl: [0; 3],
            quant_tbl: [0; 3],
            huff_tables: [
                HuffTable::new(),
                HuffTable::new(),
                HuffTable::new(),
                HuffTable::new(),
            ],
            quant_tables: [
                QuantTable::new(),
                QuantTable::new(),
                QuantTable::new(),
                QuantTable::new(),
            ],
            huff_table_count: 0,
            quant_table_count: 0,
        }
    }
}

pub fn parse_jpeg_header(data: &[u8]) -> Result<JpegHeaderInfo, &'static str> {
    let mut i = 0;
    let mut header_info = JpegHeaderInfo::new();

    while i < data.len().saturating_sub(1) {
        if data[i] == 0xFF {
            let marker = data[i + 1];

            if marker == 0xFF {
                i += 1;
                continue;
            }
            if marker == 0x00 {
                i += 2;
                continue;
            }

            match marker {
                0xC0 => {
                    let end = segment_end(data, i, "SOF")?;
                    let length = end - (i + 2);
                    if length < 8 {
                        return Err("SOF too short");
                    }
                    if data[i + 4] != 8 {
                        return Err("only 8-bit baseline JPEG is supported");
                    }

                    header_info.height = ((data[i + 5] as u32) << 8) | (data[i + 6] as u32);
                    header_info.width = ((data[i + 7] as u32) << 8) | (data[i + 8] as u32);
                    let num_components = data[i + 9] as usize;
                    if !matches!(num_components, 1 | 3) {
                        return Err("only grayscale and three-component JPEG are supported");
                    }
                    let expected_length = 8usize
                        .checked_add(
                            num_components
                                .checked_mul(3)
                                .ok_or("SOF component length overflow")?,
                        )
                        .ok_or("SOF component length overflow")?;
                    if length != expected_length {
                        return Err("SOF component payload has an invalid length");
                    }
                    header_info.num_components = num_components as u32;

                    let comp_start = i + 10;
                    for component in 0..num_components {
                        let quant_idx = data[comp_start + component * 3 + 2] as usize;
                        if quant_idx >= header_info.quant_tables.len() {
                            return Err("SOF quantization table index is out of range");
                        }
                        header_info.quant_tbl[component] = quant_idx;
                    }

                    if num_components == 3 {
                        let sampling = |component: usize| {
                            let value = data[comp_start + component * 3 + 1];
                            ((value >> 4) & 0x0f, value & 0x0f)
                        };
                        let y = sampling(0);
                        let cb = sampling(1);
                        let cr = sampling(2);
                        if cb != (1, 1) || cr != (1, 1) {
                            return Err("unsupported JPEG chroma sampling factors");
                        }
                        header_info.format = match y {
                            (2, 2) => FORMAT_420,
                            (2, 1) => FORMAT_422,
                            (1, 2) => FORMAT_224,
                            (1, 1) => FORMAT_444,
                            _ => return Err("unsupported JPEG luma sampling factors"),
                        };
                    } else {
                        let sampling = data[comp_start + 1];
                        if sampling != 0x11 {
                            return Err("unsupported grayscale sampling factors");
                        }
                        header_info.format = FORMAT_400;
                    }

                    i = end;
                    continue;
                }
                0xC2 => return Err("progressive JPEG is unsupported"),
                0xC4 => {
                    let end = segment_end(data, i, "DHT")?;
                    parse_dht(data, i + 4, end, &mut header_info)?;
                    i = end;
                    continue;
                }
                0xDA => {
                    let end = segment_end(data, i, "SOS")?;
                    if end == data.len() {
                        return Err("SOS has no entropy-coded data");
                    }
                    let sos_length = end - (i + 2);
                    if sos_length < 6 {
                        return Err("SOS too short");
                    }
                    let num_scan_components = data[i + 4] as usize;
                    if num_scan_components != header_info.num_components as usize
                        || !matches!(num_scan_components, 1 | 3)
                    {
                        return Err("SOS components do not match SOF");
                    }
                    let expected_length = 6usize
                        .checked_add(
                            num_scan_components
                                .checked_mul(2)
                                .ok_or("SOS component length overflow")?,
                        )
                        .ok_or("SOS component length overflow")?;
                    if sos_length != expected_length {
                        return Err("SOS component payload has an invalid length");
                    }

                    let mut comp_offset = i + 5;
                    for comp_idx in 0..num_scan_components {
                        let tables = data[comp_offset + 1];
                        let dc = ((tables >> 4) & 0x0f) as usize;
                        let ac = (tables & 0x0f) as usize;
                        if dc > 1 || ac > 1 {
                            return Err("SOS Huffman table index is out of range");
                        }
                        header_info.dc_huff_tbl[comp_idx] = dc;
                        header_info.ac_huff_tbl[comp_idx] = ac;
                        comp_offset += 2;
                    }

                    if data[comp_offset] != 0
                        || data[comp_offset + 1] != 63
                        || data[comp_offset + 2] != 0
                    {
                        return Err("non-baseline SOS parameters are unsupported");
                    }
                    header_info.ecs_offset = end;
                    return Ok(header_info);
                }
                0xDB => {
                    let end = segment_end(data, i, "DQT")?;
                    parse_dqt(data, i + 4, end, &mut header_info)?;
                    i = end;
                    continue;
                }
                0xDD => {
                    let end = segment_end(data, i, "DRI")?;
                    if end - (i + 2) != 4 {
                        return Err("DRI has an invalid length");
                    }
                    header_info.restart_interval =
                        ((data[i + 4] as u32) << 8) | (data[i + 5] as u32);
                    i = end;
                    continue;
                }
                0xD8 => {
                    i += 2;
                    continue;
                }
                0xD9 => break,
                _ => {
                    if marker >= 0xC0 && i + 3 < data.len() {
                        i = segment_end(data, i, "JPEG marker")?;
                        continue;
                    }
                    i += 2;
                    continue;
                }
            }
        }
        i += 1;
    }

    Err("SOS not found")
}

fn segment_end(
    data: &[u8],
    marker_offset: usize,
    marker_name: &'static str,
) -> Result<usize, &'static str> {
    let length_offset = marker_offset
        .checked_add(2)
        .ok_or("JPEG marker offset overflow")?;
    let length_bytes = data
        .get(length_offset..length_offset + 2)
        .ok_or(match marker_name {
            "SOS" => "SOS payload exceeds JPEG stream",
            _ => "JPEG marker length is truncated",
        })?;
    let length = ((length_bytes[0] as usize) << 8) | length_bytes[1] as usize;
    if length < 2 {
        return Err("JPEG marker length is invalid");
    }
    let end = length_offset
        .checked_add(length)
        .ok_or("JPEG marker length overflow")?;
    if end > data.len() {
        return Err(match marker_name {
            "SOS" => "SOS payload exceeds JPEG stream",
            _ => "JPEG marker payload exceeds stream",
        });
    }
    Ok(end)
}

fn parse_dht(
    data: &[u8],
    start: usize,
    end: usize,
    header_info: &mut JpegHeaderInfo,
) -> Result<(), &'static str> {
    if start > end || end > data.len() {
        return Err("DHT payload exceeds JPEG stream");
    }
    let mut offset = start;

    while offset < end {
        let counts_end = offset.checked_add(17).ok_or("DHT table length overflow")?;
        if counts_end > end {
            return Err("DHT table counts are truncated");
        }
        let tc_th = data[offset];
        let tc = (tc_th >> 4) & 0x0F;
        let th = tc_th & 0x0F;
        if tc > 1 || th > 1 {
            return Err("DHT table class or index is out of range");
        }
        let table_idx = ((th << 1) | tc) as usize;

        let bits = &data[offset + 1..counts_end];
        if tc == 0 && bits[12..].iter().any(|&count| count != 0) {
            return Err("DC Huffman code length exceeds the hardware table");
        }
        let num_values = bits.iter().map(|&count| count as usize).sum::<usize>();
        let hardware_limit = if tc == 0 { 12 } else { 162 };
        if num_values > hardware_limit {
            return Err("DHT symbol count exceeds the baseline hardware table");
        }
        if num_values > header_info.huff_tables[table_idx].values.len() {
            return Err("DHT defines more than 256 values");
        }
        let values_end = counts_end
            .checked_add(num_values)
            .ok_or("DHT values length overflow")?;
        if values_end > end {
            return Err("DHT values are truncated");
        }

        let table = &mut header_info.huff_tables[table_idx];
        table.bits.copy_from_slice(bits);
        table.values[..num_values].copy_from_slice(&data[counts_end..values_end]);
        table.num_values = num_values;
        table.generate();

        if table_idx >= header_info.huff_table_count {
            header_info.huff_table_count = table_idx + 1;
        }

        offset = values_end;
    }

    Ok(())
}

fn parse_dqt(
    data: &[u8],
    start: usize,
    end: usize,
    header_info: &mut JpegHeaderInfo,
) -> Result<(), &'static str> {
    if start > end || end > data.len() {
        return Err("DQT payload exceeds JPEG stream");
    }
    let mut offset = start;

    while offset < end {
        let pq_tq = data[offset];
        let precision = pq_tq >> 4;
        let tq: usize = (pq_tq & 0x0F) as usize;
        if tq >= header_info.quant_tables.len() {
            return Err("DQT table index is out of range");
        }
        let element_bytes = match precision {
            0 => 1,
            1 => 2,
            _ => return Err("DQT precision is unsupported"),
        };
        let values_len = 64usize
            .checked_mul(element_bytes)
            .ok_or("DQT values length overflow")?;
        let next = offset
            .checked_add(1)
            .and_then(|value| value.checked_add(values_len))
            .ok_or("DQT table length overflow")?;
        if next > end {
            return Err("DQT values are truncated");
        }

        if precision == 0 {
            for j in 0..64 {
                header_info.quant_tables[tq].values[j] = data[offset + 1 + j] as u16;
            }
        } else {
            for j in 0..64 {
                header_info.quant_tables[tq].values[j] = ((data[offset + 1 + j * 2] as u16) << 8)
                    | (data[offset + 1 + j * 2 + 1] as u16);
            }
        }
        offset = next;

        if tq >= header_info.quant_table_count {
            header_info.quant_table_count = tq + 1;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{JpegHeaderInfo, parse_dht, parse_jpeg_header};

    #[test]
    fn parses_baseline_yuv420_frame_and_scan_headers() {
        let baseline = [
            0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x10, 0x00, 0x10, 0x03, 0x01, 0x22,
            0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01, 0xff, 0xda, 0x00, 0x0c, 0x03, 0x01, 0x00,
            0x02, 0x11, 0x03, 0x11, 0x00, 0x3f, 0x00, 0x00,
        ];

        let header = parse_jpeg_header(&baseline).expect("baseline headers are supported");
        assert_eq!((header.width, header.height), (16, 16));
        assert_eq!(header.format, super::FORMAT_420);
        assert_eq!(header.ecs_offset, 35);
        assert_eq!(header.quant_tbl, [0, 1, 1]);
        assert_eq!(header.dc_huff_tbl, [0, 1, 1]);
        assert_eq!(header.ac_huff_tbl, [0, 1, 1]);
    }

    #[test]
    fn rejects_huffman_symbol_counts_beyond_baseline_hardware_tables() {
        let mut dc = [0u8; 30];
        dc[1] = 13;
        assert!(parse_dht(&dc, 0, dc.len(), &mut JpegHeaderInfo::new()).is_err());

        let mut ac = [0u8; 180];
        ac[0] = 0x10;
        ac[1] = 163;
        assert!(parse_dht(&ac, 0, ac.len(), &mut JpegHeaderInfo::new()).is_err());
    }

    #[test]
    fn rejects_dc_huffman_codes_longer_than_the_hardware_table() {
        let mut dc = [0u8; 18];
        dc[13] = 1;

        assert!(parse_dht(&dc, 0, dc.len(), &mut JpegHeaderInfo::new()).is_err());
    }

    #[test]
    fn rejects_quantization_table_indices_outside_hardware_range() {
        let mut malformed = [0u8; 69];
        malformed[..5].copy_from_slice(&[0xff, 0xdb, 0x00, 0x43, 0x04]);

        assert!(parse_jpeg_header(&malformed).is_err());
    }

    #[test]
    fn rejects_huffman_tables_with_more_than_256_values() {
        let mut malformed = [0u8; 278];
        malformed[..5].copy_from_slice(&[0xff, 0xc4, 0x01, 0x14, 0x00]);
        malformed[5] = 255;
        malformed[6] = 2;

        assert!(parse_jpeg_header(&malformed).is_err());
    }

    #[test]
    fn rejects_progressive_jpeg() {
        let progressive = [
            0xff, 0xc2, 0x00, 0x0b, 0x08, 0x00, 0x08, 0x00, 0x08, 0x01, 0x01, 0x11, 0x00, 0xff,
            0xda, 0x00, 0x06, 0x01, 0x01, 0x00, 0x00, 0x3f, 0x00, 0x00,
        ];

        assert!(parse_jpeg_header(&progressive).is_err());
    }

    #[test]
    fn rejects_sos_payload_that_ends_beyond_the_input() {
        let malformed = [0xff, 0xda, 0xff, 0xff];

        assert_eq!(
            parse_jpeg_header(&malformed).err(),
            Some("SOS payload exceeds JPEG stream")
        );
    }

    #[test]
    fn rejects_sos_without_any_entropy_coded_byte() {
        let no_entropy_data = [0xff, 0xda, 0x00, 0x02];

        assert_eq!(
            parse_jpeg_header(&no_entropy_data).err(),
            Some("SOS has no entropy-coded data")
        );
    }
}

//! Build the RK3588 JPEG decoder register array and table buffer from a parsed
//! [`JpegInfo`], reproducing the vendor MPP `hal_jpegd_rkv` programming.
//!
//! [`build_reg_array`] fills the geometry / format / control registers; the DMA
//! address slots (`reg9..reg13`) are left zero for the runtime to fill with
//! physical addresses. [`build_table_buffer`] writes the 1280-byte quantization
//! + Huffman table buffer the hardware reads via `reg9/10/11`.

use crate::parser::{JpegInfo, YuvMode};
use crate::registers;

/// Total table buffer size (quant + Huffman), bytes.
pub const TABLE_SIZE: usize = 1280;
/// Offset of the quantization tables within the table buffer.
pub const QUANT_TBL_OFFSET: usize = 0;
/// Offset of the Huffman min-code tables within the table buffer.
pub const MINCODE_TBL_OFFSET: usize = 384;
/// Offset of the Huffman value tables within the table buffer.
pub const VALUE_TBL_OFFSET: usize = 704;

const QUANT_LEN: usize = 64;

/// JPEG zig-zag → raster scan order (de-zig-zag mapping for quant tables).
const ZZ_ORDER: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Map a parsed chroma subsampling mode to the hardware `jpeg_mode` code.
pub fn hw_jpeg_mode(mode: YuvMode) -> u32 {
    match mode {
        YuvMode::Yuv400 => registers::JPEG_MODE_400,
        YuvMode::Yuv411 => registers::JPEG_MODE_411,
        YuvMode::Yuv420 => registers::JPEG_MODE_420,
        YuvMode::Yuv422 => registers::JPEG_MODE_422,
        YuvMode::Yuv440 => registers::JPEG_MODE_440,
        YuvMode::Yuv444 => registers::JPEG_MODE_444,
    }
}

/// Build the geometry/format/control register array for a baseline decode.
///
/// Address registers (`reg9..reg13`) are left zero; the runtime fills them with
/// physical addresses (quant table, Huffman min-code/value, stream, output).
/// Targets the native semi-planar (NV12) output path: no scaling, raster order.
pub fn build_reg_array(info: &JpegInfo) -> [u32; registers::REG_COUNT] {
    let mut regs = [0u32; registers::REG_COUNT];
    let jpeg_mode = hw_jpeg_mode(info.yuv_mode);

    // reg1: start the decoder, enable timeout + buffer-empty detection.
    regs[registers::REG_INT] =
        registers::INT_DEC_E | registers::INT_TIMEOUT_E | registers::INT_BUF_EMPTY_E;

    // reg2 (sys): native output (yuv_out_format=0), raster (dec_out_sequence=0),
    // no scaledown. 4:2:0 native always sets fill_down_e; else use parsed flags.
    let fill_down = info.yuv_mode == YuvMode::Yuv420 || info.fill_bottom;
    let mut reg2 = 0u32;
    if fill_down {
        reg2 |= 1 << 24;
    }
    if info.fill_right {
        reg2 |= 1 << 25;
    }
    regs[registers::REG_SYS] = reg2;

    // reg3: picture size (width-1, height-1).
    let width_m1 = info.width.wrapping_sub(1) as u32 & 0xffff;
    let height_m1 = info.height.wrapping_sub(1) as u32 & 0xffff;
    regs[registers::REG_PIC_SIZE] = width_m1 | (height_m1 << 16);

    // Table selectors (TBL_ENTRY_*), per hal_jpegd_rkv.
    let (qtables_sel, htables_sel) = if info.nb_components > 1 {
        let q = if info.qtbl_entry > 1 { 3 } else { 2 };
        let h = if info.htbl_entry > 0x0f { 3 } else { 2 };
        (q, h)
    } else {
        (1u32, 1u32)
    };

    // reg4: jpeg mode + pixel depth (8-bit=0) + table selects + restart interval.
    let mut reg4 = jpeg_mode & 0x7;
    reg4 |= (qtables_sel & 0x3) << 8;
    reg4 |= (htables_sel & 0x3) << 12;
    if info.restart_interval != 0 {
        reg4 |= 1 << 15; // dri_e
        reg4 |= ((info.restart_interval - 1) as u32 & 0xffff) << 16;
    }
    regs[registers::REG_PIC_FMT] = reg4;

    // Stride math (native, no scaledown, raster).
    let out_width = align_up(info.width as u32, 16);
    let out_height = if fill_down {
        align_up(info.height as u32, 16)
    } else {
        align_up(info.height as u32, 8)
    };
    let y_hor_stride = out_width >> 4;
    let uv_hor_virstride = match jpeg_mode {
        registers::JPEG_MODE_440 | registers::JPEG_MODE_444 => y_hor_stride * 2,
        registers::JPEG_MODE_411 => y_hor_stride >> 1,
        registers::JPEG_MODE_400 => 0,
        _ => y_hor_stride, // 4:2:0, 4:2:2
    };
    let y_virstride = y_hor_stride * out_height;

    regs[registers::REG_HOR_VIRSTRIDE] =
        (y_hor_stride & 0xffff) | ((uv_hor_virstride & 0xffff) << 16);
    // y_virstride field occupies bits [31:4].
    regs[registers::REG_Y_VIRSTRIDE] = (y_virstride & 0x0fff_ffff) << 4;

    // reg7: table lengths + high bit of y_hor_stride.
    let qtbl_len = if info.nb_components > 0 {
        qtables_sel * 8 - 1
    } else {
        0
    };
    let (htbl_mincode_len, htbl_value_len) = match htables_sel {
        0 => (0, 0),
        2 => ((info.nb_components as u32 - 1) * 6 - 1, htables_sel * 12 - 1),
        _ => (info.nb_components as u32 * 6 - 1, htables_sel * 12 - 1),
    };
    let y_hor_virstride_h = (y_hor_stride >> 16) & 1;
    regs[registers::REG_TABLE_LEN] = (qtbl_len & 0x1f)
        | ((htbl_mincode_len & 0x1f) << 8)
        | ((htbl_value_len & 0x3f) << 16)
        | (y_hor_virstride_h << 24);

    // reg8: stream length (16-byte units, minus one) + start byte.
    let hw_strm_offset = info.strm_offset - info.strm_offset % 16;
    let start_byte = info.strm_offset % 16;
    let stream_len = (align_up(info.pkt_len - hw_strm_offset, 16) - 1) >> 4;
    regs[registers::REG_STRM_LEN] = (start_byte & 0xf) | ((stream_len & 0x0fff_ffff) << 4);

    // reg14: stream error handling defaults (error_prc_mode=1, skip 0xffff/other marks).
    regs[registers::REG_ERR_MODE] = 1 | (2 << 5) | (2 << 7);

    // reg16: enable all clock gates.
    regs[16] = 0xff;

    // reg30: AXI perf counters (matches vendor; inert for decode correctness).
    regs[30] = 1 | (1 << 1) | (1 << 3) | (0xa << 4);

    regs
}

/// Write the 1280-byte quantization + Huffman table buffer the hardware reads.
pub fn build_table_buffer(info: &JpegInfo, out: &mut [u8; TABLE_SIZE]) {
    *out = [0; TABLE_SIZE];
    write_quant_tables(info, out);
    write_huffman_tables(info, out);
}

fn write_quant_tables(info: &JpegInfo, out: &mut [u8; TABLE_SIZE]) {
    for j in 0..info.nb_components as usize {
        let idx = info.components[j].quant_index as usize;
        let mut raster = [0u16; QUANT_LEN];
        for i in 0..QUANT_LEN {
            raster[ZZ_ORDER[i]] = info.quant_tables[idx][i];
        }
        let base = QUANT_TBL_OFFSET + j * QUANT_LEN * 2;
        for (i, v) in raster.iter().enumerate() {
            put_u16(out, base + i * 2, *v);
        }
    }
}

fn write_huffman_tables(info: &JpegInfo, out: &mut [u8; TABLE_SIZE]) {
    for k in 0..info.nb_components as usize {
        // Component 0 uses table selectors [0]; components 1 and 2 share [1].
        let sel = if k == 0 { 0 } else { 1 };
        let dc = &info.dc_tables[info.components[sel].dc_index as usize];
        let ac = &info.ac_tables[info.components[sel].ac_index as usize];

        let (min_dc, acc_dc) = huff_min_code(&dc.bits);
        let (min_ac, acc_ac) = huff_min_code(&ac.bits);

        let mut p = MINCODE_TBL_OFFSET + k * 96;
        for &v in &min_dc {
            put_u16(out, p, v);
            p += 2;
        }
        for i in 0..8 {
            put_u16(out, p, acc_dc[2 * i] | (acc_dc[2 * i + 1] << 8));
            p += 2;
        }
        for &v in &min_ac {
            put_u16(out, p, v);
            p += 2;
        }
        for i in 0..8 {
            put_u16(out, p, acc_ac[2 * i] | (acc_ac[2 * i + 1] << 8));
            p += 2;
        }

        let vbase = VALUE_TBL_OFFSET + k * 192;
        let mut hv = [0u8; 192];
        hv[..12].copy_from_slice(&dc.vals);
        hv[16..16 + 162].copy_from_slice(&ac.vals);
        out[vbase..vbase + 192].copy_from_slice(&hv);
    }
}

/// Canonical Huffman min-code + accumulated-address generation, matching the
/// vendor `jpegd_vpu7xx_write_htbl` (16-bit wrapping arithmetic).
fn huff_min_code(bits: &[u8; 16]) -> ([u16; 16], [u16; 16]) {
    let mut min_code = [0u16; 16];
    let mut acc_addr = [0u16; 16];
    let mut code: u16 = 0;
    let mut addr: u16 = 0;
    for j in 0..16 {
        let len = bits[j] as u16;
        if len == 0 && j > 0 {
            let cand = min_code[j - 1] << 1;
            min_code[j] = if code > cand { code } else { cand };
        } else {
            min_code[j] = code;
        }
        code = code.wrapping_add(len);
        addr = addr.wrapping_add(len);
        acc_addr[j] = addr;
        code <<= 1;
    }
    if bits[15] != 0 {
        min_code[0] = min_code[15].wrapping_add(bits[15] as u16).wrapping_sub(1);
    } else {
        min_code[0] = min_code[15];
    }
    (min_code, acc_addr)
}

fn put_u16(out: &mut [u8], pos: usize, val: u16) {
    out[pos] = (val & 0xff) as u8;
    out[pos + 1] = (val >> 8) as u8;
}

fn align_up(v: u32, a: u32) -> u32 {
    (v + a - 1) & !(a - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{AcHuffTable, Component, DcHuffTable};

    const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    const DC_LUMA_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    const AC_SMALL_BITS: [u8; 16] = [0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    include!("test_expected_table.rs");

    /// A 64x48 baseline 4:2:0 fixture matching `parser::tests::parse_420` and the
    /// Python-generated `EXPECTED_TABLE`. strm_offset/pkt_len chosen to exercise
    /// the stream-length math.
    fn fixture_420() -> JpegInfo {
        let mut info = JpegInfo::zeroed();
        info.width = 64;
        info.height = 48;
        info.nb_components = 3;
        info.yuv_mode = YuvMode::Yuv420;
        info.h_max = 2;
        info.v_max = 2;
        info.qtbl_entry = 2;
        info.htbl_entry = 0x0f;
        info.restart_interval = 0;
        info.strm_offset = 44;
        info.pkt_len = 200;
        info.components[0] = Component {
            id: 1,
            h: 2,
            v: 2,
            quant_index: 0,
            dc_index: 0,
            ac_index: 0,
        };
        info.components[1] = Component {
            id: 2,
            h: 1,
            v: 1,
            quant_index: 1,
            dc_index: 1,
            ac_index: 1,
        };
        info.components[2] = Component {
            id: 3,
            h: 1,
            v: 1,
            quant_index: 1,
            dc_index: 1,
            ac_index: 1,
        };
        for i in 0..64 {
            info.quant_tables[0][i] = (i + 1) as u16;
            info.quant_tables[1][i] = (100 + i) as u16;
        }
        let mut dc = DcHuffTable {
            bits: DC_LUMA_BITS,
            vals: [0; 12],
        };
        dc.vals = DC_LUMA_VALS;
        let mut ac = AcHuffTable {
            bits: AC_SMALL_BITS,
            vals: [0; 162],
        };
        ac.vals[0] = 0x01;
        ac.vals[1] = 0x02;
        info.dc_tables[0] = dc;
        info.dc_tables[1] = dc;
        info.ac_tables[0] = ac;
        info.ac_tables[1] = ac;
        info
    }

    #[test]
    fn jpeg_mode_mapping() {
        assert_eq!(hw_jpeg_mode(YuvMode::Yuv400), registers::JPEG_MODE_400);
        assert_eq!(hw_jpeg_mode(YuvMode::Yuv411), registers::JPEG_MODE_411);
        assert_eq!(hw_jpeg_mode(YuvMode::Yuv420), registers::JPEG_MODE_420);
        assert_eq!(hw_jpeg_mode(YuvMode::Yuv422), registers::JPEG_MODE_422);
        assert_eq!(hw_jpeg_mode(YuvMode::Yuv440), registers::JPEG_MODE_440);
        assert_eq!(hw_jpeg_mode(YuvMode::Yuv444), registers::JPEG_MODE_444);
    }

    #[test]
    fn reg_array_control_and_geometry() {
        let regs = build_reg_array(&fixture_420());
        // reg1: dec_e | dec_timeout_e | buf_empty_e
        assert_eq!(regs[registers::REG_INT], 0x0000_000D);
        // reg2: fill_down_e (bit24) for 4:2:0; nothing else.
        assert_eq!(regs[registers::REG_SYS], 0x0100_0000);
        // reg3: (width-1) | (height-1)<<16
        assert_eq!(regs[registers::REG_PIC_SIZE], 63 | (47 << 16));
        // reg4: jpeg_mode=2, qtables_sel=3, htables_sel=2
        assert_eq!(regs[registers::REG_PIC_FMT], 2 | (3 << 8) | (2 << 12));
        // reg5: y_hor_stride=4, uv_hor_virstride=4
        assert_eq!(regs[registers::REG_HOR_VIRSTRIDE], 4 | (4 << 16));
        // reg6: y_virstride=192, stored at bits[31:4]
        assert_eq!(regs[registers::REG_Y_VIRSTRIDE], 192 << 4);
        // reg7: qtbl_len=23, htbl_mincode_len=11, htbl_value_len=23, y_hor_virstride_h=0
        assert_eq!(regs[registers::REG_TABLE_LEN], 23 | (11 << 8) | (23 << 16));
        // reg8: start_byte=12, stream_len=10
        assert_eq!(regs[registers::REG_STRM_LEN], 12 | (10 << 4));
        // reg14: error_prc_mode=1, ffff_err_mode=2<<5, other_mark_mode=2<<7
        assert_eq!(regs[registers::REG_ERR_MODE], 0x141);
        // reg16: all clock gates on
        assert_eq!(regs[16], 0xff);
        // reg30: AXI perf control (vendor default)
        assert_eq!(regs[30], 1 | (1 << 1) | (1 << 3) | (0xa << 4));
    }

    #[test]
    fn reg_array_leaves_address_slots_zero() {
        let regs = build_reg_array(&fixture_420());
        assert_eq!(regs[registers::REG_QTBL_BASE], 0);
        assert_eq!(regs[registers::REG_HUFFMIN_BASE], 0);
        assert_eq!(regs[registers::REG_HUFFVAL_BASE], 0);
        assert_eq!(regs[registers::REG_STRM_BASE], 0);
        assert_eq!(regs[registers::REG_DEC_OUT_BASE], 0);
    }

    #[test]
    fn reg4_sets_restart_interval() {
        let mut info = fixture_420();
        info.restart_interval = 8;
        let regs = build_reg_array(&info);
        // dri_e (bit15) | dri_mcu_num_m1 (7) << 16, plus existing reg4 fields.
        let expected = 2 | (3 << 8) | (2 << 12) | (1 << 15) | (7 << 16);
        assert_eq!(regs[registers::REG_PIC_FMT], expected);
    }

    #[test]
    fn table_buffer_matches_vendor_layout() {
        let mut out = [0u8; TABLE_SIZE];
        build_table_buffer(&fixture_420(), &mut out);
        assert_eq!(&out[..], &EXPECTED_TABLE[..]);
    }

    #[test]
    fn quant_table_is_de_zigzagged() {
        let mut out = [0u8; TABLE_SIZE];
        build_table_buffer(&fixture_420(), &mut out);
        // Component 0 quantization: raster[zz[i]] == quant_stream[i] == i+1.
        for (i, &zz) in ZZ_ORDER.iter().enumerate() {
            let pos = zz * 2;
            let val = u16::from_le_bytes([out[pos], out[pos + 1]]);
            assert_eq!(val, (i + 1) as u16);
        }
    }
}

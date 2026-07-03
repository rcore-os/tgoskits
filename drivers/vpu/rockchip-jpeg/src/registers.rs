//! RK3588 JPEG decoder (VDPU720 / `RKDJPEG`) register definitions.
//!
//! The hardware exposes a flat 42-word register file (`RKDJPEG_SWREG0..41`) in a
//! 0x400 MMIO window. Register `N` lives at byte offset `N * 4`. The vendor MPP
//! `hal_jpegd_rkv` driver programs the same file as a `[u32; 42]` array, so this
//! driver mirrors that index-addressed model: encoders build a `[u32; REG_COUNT]`
//! and the MMIO layer writes index `i` to offset `i * 4`.

/// Number of 32-bit registers in the JPEG decoder register file.
pub const REG_COUNT: usize = 42;

/// Byte size of the MMIO register window (`reg = <... 0x400>` in the DTS).
pub const MMIO_WINDOW: usize = 0x400;

/// Byte offset of register index `i`.
pub const fn offset(index: usize) -> usize {
    index * 4
}

// --- Register indices (index == SWREG number == offset / 4) ---

/// `SWREG0` — ID / version (read-only).
pub const REG_ID: usize = 0;
/// `SWREG1` — interrupt + control (start, soft-reset, status).
pub const REG_INT: usize = 1;
/// `SWREG2` — system / output-format control.
pub const REG_SYS: usize = 2;
/// `SWREG3` — picture size (`width-1`, `height-1`).
pub const REG_PIC_SIZE: usize = 3;
/// `SWREG4` — picture format (jpeg mode, table selects, restart interval).
pub const REG_PIC_FMT: usize = 4;
/// `SWREG5` — horizontal virtual strides (Y + UV), in 16-byte units.
pub const REG_HOR_VIRSTRIDE: usize = 5;
/// `SWREG6` — Y plane virtual stride.
pub const REG_Y_VIRSTRIDE: usize = 6;
/// `SWREG7` — quant / Huffman table lengths.
pub const REG_TABLE_LEN: usize = 7;
/// `SWREG8` — stream length + start byte.
pub const REG_STRM_LEN: usize = 8;
/// `SWREG9` — quantization-table DMA base (64-byte aligned).
pub const REG_QTBL_BASE: usize = 9;
/// `SWREG10` — Huffman min-code table DMA base (64-byte aligned).
pub const REG_HUFFMIN_BASE: usize = 10;
/// `SWREG11` — Huffman value table DMA base (64-byte aligned).
pub const REG_HUFFVAL_BASE: usize = 11;
/// `SWREG12` — input bitstream DMA base (16-byte aligned).
pub const REG_STRM_BASE: usize = 12;
/// `SWREG13` — single output (decode result) DMA base (64-byte aligned).
pub const REG_DEC_OUT_BASE: usize = 13;
/// `SWREG14` — error processing mode.
pub const REG_ERR_MODE: usize = 14;
/// `SWREG33` — detailed stream/Huffman error info (read-only; dump on failure).
pub const REG_DBG_ERR: usize = 33;

/// Register-file slots that hold DMA addresses (translated by the MPP ABI layer).
/// Matches the vendor `trans_tbl_jpgdec` table.
pub const ADDR_REG_INDICES: &[usize] = &[
    REG_QTBL_BASE,
    REG_HUFFMIN_BASE,
    REG_HUFFVAL_BASE,
    REG_STRM_BASE,
    REG_DEC_OUT_BASE,
];

// --- SWREG1 (INT/control) bit fields ---

/// Start the decoder (`sw_dec_e`). Hardware self-clears on done/error/timeout.
pub const INT_DEC_E: u32 = 1 << 0;
/// Disable the decode-ready interrupt (`sw_dec_irq_dis`).
pub const INT_IRQ_DIS: u32 = 1 << 1;
/// Enable decode timeout detection (`sw_dec_timeout_e`).
pub const INT_TIMEOUT_E: u32 = 1 << 2;
/// Enable buffer-empty detection (`sw_buf_empty_e`).
pub const INT_BUF_EMPTY_E: u32 = 1 << 3;
/// Soft-reset pulse (`sw_softrst_en_p`, write-1).
pub const INT_SOFTRESET: u32 = 1 << 5;
/// Raw interrupt status (`sw_dec_irq_raw`); software clears this (W1C) after handling.
pub const INT_IRQ_RAW: u32 = 1 << 6;
/// Interrupt asserted (`sw_dec_irq`).
pub const INT_IRQ: u32 = 1 << 8;
/// Frame decode ready / done (`sw_dec_rdy_sta`).
pub const INT_RDY_STA: u32 = 1 << 9;
/// AXI bus error (`sw_dec_bus_sta`).
pub const INT_BUS_STA: u32 = 1 << 10;
/// Stream decode error (`sw_dec_error_sta`).
pub const INT_ERROR_STA: u32 = 1 << 11;
/// Decode timeout (`sw_dec_timeout_sta`).
pub const INT_TIMEOUT_STA: u32 = 1 << 12;
/// Stream buffer empty (`sw_dec_buf_empty_sta`).
pub const INT_BUF_EMPTY_STA: u32 = 1 << 13;
/// Soft-reset ready (`sw_softreset_rdy`).
pub const INT_SOFTRESET_RDY: u32 = 1 << 14;

/// Mask of all error status bits in `SWREG1`.
pub const INT_ERROR_MASK: u32 = INT_BUS_STA | INT_ERROR_STA | INT_TIMEOUT_STA | INT_BUF_EMPTY_STA;
/// Mask of status bits to clear (W1C) after handling completion.
pub const INT_STATUS_CLEAR_MASK: u32 = INT_IRQ_RAW
    | INT_IRQ
    | INT_RDY_STA
    | INT_BUS_STA
    | INT_ERROR_STA
    | INT_TIMEOUT_STA
    | INT_BUF_EMPTY_STA;

// --- SWREG4 (PIC_FMT) `jpeg_mode` codes (chroma subsampling, hardware encoding) ---

/// 4:0:0 (grayscale).
pub const JPEG_MODE_400: u32 = 0;
/// 4:1:1.
pub const JPEG_MODE_411: u32 = 1;
/// 4:2:0.
pub const JPEG_MODE_420: u32 = 2;
/// 4:2:2.
pub const JPEG_MODE_422: u32 = 3;
/// 4:4:0.
pub const JPEG_MODE_440: u32 = 4;
/// 4:4:4.
pub const JPEG_MODE_444: u32 = 5;

// --- SWREG2 (SYS) `yuv_out_format` codes (bits [29:27]) ---

/// Native semi-planar YUV output (NV12 for 4:2:0).
pub const OUT_FMT_NATIVE: u32 = 0;
/// Explicit NV12 (YUV420 semi-planar).
pub const OUT_FMT_NV12: u32 = 3;
/// Packed YUYV.
pub const OUT_FMT_YUYV: u32 = 4;
/// Bit position of `yuv_out_format` within `SWREG2`.
pub const OUT_FMT_SHIFT: u32 = 27;
/// `cbcr_swap` bit within `SWREG2` (selects NV21 vs NV12 ordering).
pub const SYS_CBCR_SWAP: u32 = 1 << 9;

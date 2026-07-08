//! Generates the baseline JPEG used by the boot self-test, from auditable source
//! rather than a committed binary fixture.
//!
//! The image is a 64x64 solid mid-gray (level-shifted to zero) frame in 4:2:0.
//! Every 8x8 block is therefore all-zero, so each block's entropy code is just a
//! DC-difference-of-zero followed by an end-of-block — no forward DCT is needed.
//! The Huffman and structural layout use the standard ITU-T T.81 Annex K.3 tables
//! (the same tables [`crate::parser`] installs for DHT-less streams), so the
//! result is a fully standards-conformant stream any baseline decoder — including
//! the RK3588 JPU — accepts. The output is deterministic.

use crate::parser::{
    DEFAULT_AC_CHROMA_BITS, DEFAULT_AC_CHROMA_VALS, DEFAULT_AC_LUMA_BITS, DEFAULT_AC_LUMA_VALS,
    DEFAULT_DC_CHROMA_BITS, DEFAULT_DC_LUMA_BITS, DEFAULT_DC_VALS,
};

/// Upper bound on the encoded self-test JPEG size; callers can stack-allocate a
/// buffer of this size. The actual stream is ~0.7 KiB.
pub const SELFTEST_JPEG_CAPACITY: usize = 1024;

const SELFTEST_DIM: u16 = 64;
/// End-of-block AC symbol (run 0, size 0).
const AC_EOB: u8 = 0x00;
/// DC symbol for a zero difference (category 0).
const DC_ZERO: u8 = 0x00;

/// Bounds-checked forward-only byte writer over a caller buffer.
struct Writer<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> Writer<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn push(&mut self, byte: u8) -> Option<()> {
        *self.buf.get_mut(self.pos)? = byte;
        self.pos += 1;
        Some(())
    }

    fn push_all(&mut self, bytes: &[u8]) -> Option<()> {
        for &b in bytes {
            self.push(b)?;
        }
        Some(())
    }

    fn marker(&mut self, code: u8) -> Option<()> {
        self.push(0xFF)?;
        self.push(code)
    }

    /// Write a length-prefixed marker segment (`FF <code> <len:2> <body>`).
    fn segment(&mut self, code: u8, body: &[u8]) -> Option<()> {
        self.marker(code)?;
        let len = (body.len() + 2) as u16;
        self.push((len >> 8) as u8)?;
        self.push((len & 0xff) as u8)?;
        self.push_all(body)
    }
}

/// MSB-first Huffman bit packer with JPEG `0xFF -> 0xFF 0x00` byte stuffing.
struct BitWriter {
    acc: u32,
    nbits: u32,
}

impl BitWriter {
    fn new() -> Self {
        Self { acc: 0, nbits: 0 }
    }

    fn put(&mut self, code: u16, len: u8, out: &mut Writer) -> Option<()> {
        self.acc = (self.acc << len) | u32::from(code);
        self.nbits += u32::from(len);
        while self.nbits >= 8 {
            self.nbits -= 8;
            let byte = ((self.acc >> self.nbits) & 0xFF) as u8;
            out.push(byte)?;
            if byte == 0xFF {
                out.push(0x00)?;
            }
        }
        Some(())
    }

    /// Flush the final partial byte, padding the low bits with ones (per T.81).
    fn flush(&mut self, out: &mut Writer) -> Option<()> {
        if self.nbits > 0 {
            let pad = 8 - self.nbits;
            let byte = (((self.acc << pad) | ((1 << pad) - 1)) & 0xFF) as u8;
            out.push(byte)?;
            if byte == 0xFF {
                out.push(0x00)?;
            }
            self.nbits = 0;
        }
        Some(())
    }
}

/// Canonical JPEG Huffman code (value, bit length) for `symbol` in a table given
/// by its `BITS` (codes-per-length) and `VALS` (symbol order) arrays.
fn huff_code(bits: &[u8; 16], vals: &[u8], symbol: u8) -> (u16, u8) {
    let mut code: u16 = 0;
    let mut k = 0usize;
    for (i, &count) in bits.iter().enumerate() {
        let len = (i + 1) as u8;
        for _ in 0..count {
            if vals[k] == symbol {
                return (code, len);
            }
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    // The self-test only ever asks for symbols that exist in the standard tables.
    unreachable!("symbol not present in Huffman table")
}

/// Encode the self-test baseline 4:2:0 JPEG into `out`, returning its length.
/// Returns `None` only if `out` is smaller than the encoded stream.
pub fn write_selftest_jpeg(out: &mut [u8]) -> Option<usize> {
    let mut w = Writer::new(out);

    // SOI.
    w.marker(0xD8)?;

    // Two flat 8-bit quantization tables (luma id 0, chroma id 1). The image is
    // all-zero so the divisor is irrelevant; two tables satisfy 4:2:0 selectors.
    let mut dqt = [1u8; 65];
    dqt[0] = 0x00; // Pq=0, Tq=0
    w.segment(0xDB, &dqt)?;
    dqt[0] = 0x01; // Pq=0, Tq=1
    w.segment(0xDB, &dqt)?;

    // Four Huffman tables (standard Annex K.3): DC/AC for luma (id 0) and chroma
    // (id 1). `Tc_Th`: Tc=0 DC / 1 AC in the high nibble, Th (table id) in the low.
    write_dht(&mut w, 0x00, &DEFAULT_DC_LUMA_BITS, &DEFAULT_DC_VALS)?;
    write_dht(&mut w, 0x10, &DEFAULT_AC_LUMA_BITS, &DEFAULT_AC_LUMA_VALS)?;
    write_dht(&mut w, 0x01, &DEFAULT_DC_CHROMA_BITS, &DEFAULT_DC_VALS)?;
    write_dht(
        &mut w,
        0x11,
        &DEFAULT_AC_CHROMA_BITS,
        &DEFAULT_AC_CHROMA_VALS,
    )?;

    // SOF0: 8-bit, 64x64, 3 components. Y (id 1) is 2x2 sampled with quant table 0;
    // Cb/Cr (id 2/3) are 1x1 with quant table 1 -> 4:2:0.
    let (h_hi, h_lo) = (SELFTEST_DIM.to_be_bytes()[0], SELFTEST_DIM.to_be_bytes()[1]);
    let sof0 = [
        0x08, h_hi, h_lo, h_hi, h_lo, 0x03, // precision, H, W, num components
        0x01, 0x22, 0x00, // Y : id 1, H=2 V=2, Tq=0
        0x02, 0x11, 0x01, // Cb: id 2, H=1 V=1, Tq=1
        0x03, 0x11, 0x01, // Cr: id 3, H=1 V=1, Tq=1
    ];
    w.segment(0xC0, &sof0)?;

    // SOS: Y uses DC/AC table 0, Cb/Cr use table 1; full baseline spectral range.
    let sos = [
        0x03, // num components
        0x01, 0x00, // Y : Td=0, Ta=0
        0x02, 0x11, // Cb: Td=1, Ta=1
        0x03, 0x11, // Cr: Td=1, Ta=1
        0x00, 0x3F, 0x00, // Ss=0, Se=63, Ah/Al=0
    ];
    w.segment(0xDA, &sos)?;

    // Entropy-coded scan. Precompute the two codes each block needs; every block
    // is all-zero, so it is exactly a zero DC difference plus an end-of-block.
    let dc_luma = huff_code(&DEFAULT_DC_LUMA_BITS, &DEFAULT_DC_VALS, DC_ZERO);
    let ac_luma = huff_code(&DEFAULT_AC_LUMA_BITS, &DEFAULT_AC_LUMA_VALS, AC_EOB);
    let dc_chroma = huff_code(&DEFAULT_DC_CHROMA_BITS, &DEFAULT_DC_VALS, DC_ZERO);
    let ac_chroma = huff_code(&DEFAULT_AC_CHROMA_BITS, &DEFAULT_AC_CHROMA_VALS, AC_EOB);

    // 64x64 in 4:2:0 is 4x4 = 16 MCUs, each with 4 Y blocks + 1 Cb + 1 Cr.
    let mcus = (SELFTEST_DIM as usize / 16) * (SELFTEST_DIM as usize / 16);
    let mut bits = BitWriter::new();
    for _ in 0..mcus {
        for _ in 0..4 {
            bits.put(dc_luma.0, dc_luma.1, &mut w)?;
            bits.put(ac_luma.0, ac_luma.1, &mut w)?;
        }
        bits.put(dc_chroma.0, dc_chroma.1, &mut w)?;
        bits.put(ac_chroma.0, ac_chroma.1, &mut w)?;
        bits.put(dc_chroma.0, dc_chroma.1, &mut w)?;
        bits.put(ac_chroma.0, ac_chroma.1, &mut w)?;
    }
    bits.flush(&mut w)?;

    // EOI.
    w.marker(0xD9)?;
    Some(w.pos)
}

fn write_dht(w: &mut Writer, tc_th: u8, bits: &[u8; 16], vals: &[u8]) -> Option<()> {
    let count: usize = bits.iter().map(|&b| b as usize).sum();
    let mut body = [0u8; 1 + 16 + crate::parser::MAX_AC_VALS];
    body[0] = tc_th;
    body[1..17].copy_from_slice(bits);
    body[17..17 + count].copy_from_slice(&vals[..count]);
    w.segment(0xC4, &body[..17 + count])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{YuvMode, parse};

    #[test]
    fn selftest_jpeg_parses_as_baseline_420() {
        let mut buf = [0u8; SELFTEST_JPEG_CAPACITY];
        let n = write_selftest_jpeg(&mut buf).expect("buffer large enough");
        let info = parse(&buf[..n]).expect("generated self-test JPEG must parse");
        assert_eq!((info.width, info.height), (64, 64));
        assert_eq!(info.nb_components, 3);
        assert_eq!(info.yuv_mode, YuvMode::Yuv420);
        assert_eq!(info.qtbl_entry, 2);
        assert_eq!(info.htbl_entry, 0x0f);
        assert!(info.strm_offset > 0 && (info.strm_offset as usize) < info.pkt_len as usize);
    }

    #[test]
    fn selftest_jpeg_is_wrapped_in_soi_eoi() {
        let mut buf = [0u8; SELFTEST_JPEG_CAPACITY];
        let n = write_selftest_jpeg(&mut buf).unwrap();
        assert_eq!(&buf[..2], &[0xFF, 0xD8], "starts with SOI");
        assert_eq!(&buf[n - 2..n], &[0xFF, 0xD9], "ends with EOI");
    }

    #[test]
    fn selftest_jpeg_reports_too_small_buffer() {
        let mut small = [0u8; 16];
        assert_eq!(write_selftest_jpeg(&mut small), None);
    }

    #[test]
    fn huff_code_matches_canonical_assignment() {
        // Standard luma DC: first length-2 code is symbol 0 -> 0b00.
        assert_eq!(
            huff_code(&DEFAULT_DC_LUMA_BITS, &DEFAULT_DC_VALS, 0),
            (0b00, 2)
        );
        // Standard chroma AC: symbol 0x00 (EOB) is the first length-2 code -> 0b00.
        assert_eq!(
            huff_code(&DEFAULT_AC_CHROMA_BITS, &DEFAULT_AC_CHROMA_VALS, AC_EOB),
            (0b00, 2)
        );
    }
}

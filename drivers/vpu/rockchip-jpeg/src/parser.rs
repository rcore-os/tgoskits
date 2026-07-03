//! Minimal baseline-sequential JPEG header parser.
//!
//! Extracts everything the RK3588 JPEG decoder needs to decode one baseline
//! frame: dimensions, component sampling (→ chroma subsampling mode), the
//! quantization tables (left in JPEG zig-zag order; the hardware table builder
//! de-zig-zags them), the Huffman tables, the restart interval, and the byte
//! offset/length of the entropy-coded scan data.
//!
//! The field layout mirrors the vendor MPP `JpegdSyntax` so the
//! [`crate::command`] table builder can reproduce `hal_jpegd_rkv` byte-for-byte.

/// Maximum components handled (JFIF Y/Cb/Cr).
pub const MAX_COMPONENTS: usize = 3;
/// Coefficients per quantization table.
pub const QUANT_LEN: usize = 64;
/// Maximum baseline DC Huffman values.
pub const MAX_DC_VALS: usize = 12;
/// Maximum baseline AC Huffman values.
pub const MAX_AC_VALS: usize = 162;
/// Number of distinct quantization tables a stream may define (Tq 0..3).
pub const NUM_QUANT_TABLES: usize = 4;
/// Number of distinct DC/AC Huffman tables this driver supports (Th 0..1, baseline).
pub const NUM_HUFF_TABLES: usize = 2;

/// Errors produced while parsing a JPEG header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Ran off the end of the input.
    Truncated,
    /// Missing the start-of-image (`FFD8`) marker.
    MissingSoi,
    /// A marker segment had an invalid length.
    BadSegment,
    /// The frame is not baseline sequential (`SOF0`).
    NotBaseline,
    /// Sample precision other than 8-bit.
    UnsupportedPrecision,
    /// More than [`MAX_COMPONENTS`] components.
    TooManyComponents,
    /// A table id outside the supported range.
    TableIdOutOfRange,
    /// No `SOF0` segment was seen before the scan.
    MissingSof,
    /// No `SOS` segment was found.
    MissingSos,
    /// The component sampling factors are not a supported subsampling.
    UnsupportedSubsampling,
}

/// Chroma subsampling mode (matches the vendor `JPEGDEC_*` semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvMode {
    /// 4:0:0 grayscale.
    Yuv400,
    /// 4:2:0.
    Yuv420,
    /// 4:2:2.
    Yuv422,
    /// 4:4:0.
    Yuv440,
    /// 4:4:4.
    Yuv444,
    /// 4:1:1.
    Yuv411,
}

/// One frame component (from `SOF0`, augmented with `SOS` table selectors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Component {
    /// Component identifier (`Ci`).
    pub id: u8,
    /// Horizontal sampling factor (`Hi`).
    pub h: u8,
    /// Vertical sampling factor (`Vi`).
    pub v: u8,
    /// Quantization table selector (`Tqi`).
    pub quant_index: u8,
    /// DC Huffman table selector (`Tdi`, from `SOS`).
    pub dc_index: u8,
    /// AC Huffman table selector (`Tai`, from `SOS`).
    pub ac_index: u8,
}

impl Component {
    const ZERO: Self = Self {
        id: 0,
        h: 0,
        v: 0,
        quant_index: 0,
        dc_index: 0,
        ac_index: 0,
    };
}

/// A DC Huffman table: code-length counts plus values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DcHuffTable {
    /// Number of codes of each length 1..16 (`BITS`).
    pub bits: [u8; 16],
    /// Symbol values (`HUFFVAL`), zero-padded.
    pub vals: [u8; MAX_DC_VALS],
}

impl DcHuffTable {
    const ZERO: Self = Self {
        bits: [0; 16],
        vals: [0; MAX_DC_VALS],
    };
}

/// An AC Huffman table: code-length counts plus values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcHuffTable {
    /// Number of codes of each length 1..16 (`BITS`).
    pub bits: [u8; 16],
    /// Symbol values (`HUFFVAL`), zero-padded.
    pub vals: [u8; MAX_AC_VALS],
}

impl AcHuffTable {
    const ZERO: Self = Self {
        bits: [0; 16],
        vals: [0; MAX_AC_VALS],
    };
}

/// Parsed baseline JPEG header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegInfo {
    /// Image width in pixels.
    pub width: u16,
    /// Image height in pixels.
    pub height: u16,
    /// Number of components.
    pub nb_components: u8,
    /// Components in `SOF0` order.
    pub components: [Component; MAX_COMPONENTS],
    /// Derived chroma subsampling mode.
    pub yuv_mode: YuvMode,
    /// Maximum horizontal sampling factor.
    pub h_max: u8,
    /// Maximum vertical sampling factor.
    pub v_max: u8,
    /// Quantization tables by id, in JPEG zig-zag order.
    pub quant_tables: [[u16; QUANT_LEN]; NUM_QUANT_TABLES],
    /// DC Huffman tables by id.
    pub dc_tables: [DcHuffTable; NUM_HUFF_TABLES],
    /// AC Huffman tables by id.
    pub ac_tables: [AcHuffTable; NUM_HUFF_TABLES],
    /// Restart interval in MCUs (0 if absent).
    pub restart_interval: u16,
    /// Number of distinct quantization tables defined (`DQT` count).
    pub qtbl_entry: u8,
    /// Bitmask of Huffman tables defined: DC id `n` → bit `2n`, AC id `n` → bit `2n+1`.
    pub htbl_entry: u8,
    /// Right-edge MCU padding required (vendor `fill_right`).
    pub fill_right: bool,
    /// Bottom-edge MCU padding required (vendor `fill_bottom`).
    pub fill_bottom: bool,
    /// Byte offset of the first entropy-coded scan byte.
    pub strm_offset: u32,
    /// Total JPEG packet length.
    pub pkt_len: u32,
}

impl JpegInfo {
    const ZERO: Self = Self {
        width: 0,
        height: 0,
        nb_components: 0,
        components: [Component::ZERO; MAX_COMPONENTS],
        yuv_mode: YuvMode::Yuv400,
        h_max: 0,
        v_max: 0,
        quant_tables: [[0; QUANT_LEN]; NUM_QUANT_TABLES],
        dc_tables: [DcHuffTable::ZERO; NUM_HUFF_TABLES],
        ac_tables: [AcHuffTable::ZERO; NUM_HUFF_TABLES],
        restart_interval: 0,
        qtbl_entry: 0,
        htbl_entry: 0,
        fill_right: false,
        fill_bottom: false,
        strm_offset: 0,
        pkt_len: 0,
    };

    /// An all-zero [`JpegInfo`] (useful for tests and incremental construction).
    pub const fn zeroed() -> Self {
        Self::ZERO
    }
}

// Standard ITU-T T.81 Annex K.3 Huffman tables, installed verbatim when a stream
// carries no DHT (common in UVC / motion-JPEG). These mirror MPP's
// `jpegd_setup_default_dht`, which fills both the luma (id 0) and chroma (id 1)
// slots and then forces all four tables "present". Byte-for-byte identical to the
// vendor tables so the [`crate::command`] table buffer matches `hal_jpegd_rkv`.
pub(crate) const DEFAULT_DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
pub(crate) const DEFAULT_DC_CHROMA_BITS: [u8; 16] =
    [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
/// Both default DC tables share the same value list (0..=11).
pub(crate) const DEFAULT_DC_VALS: [u8; MAX_DC_VALS] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

pub(crate) const DEFAULT_AC_LUMA_BITS: [u8; 16] =
    [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
#[rustfmt::skip]
pub(crate) const DEFAULT_AC_LUMA_VALS: [u8; MAX_AC_VALS] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

pub(crate) const DEFAULT_AC_CHROMA_BITS: [u8; 16] =
    [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
#[rustfmt::skip]
pub(crate) const DEFAULT_AC_CHROMA_VALS: [u8; MAX_AC_VALS] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

/// Install the standard Annex K.3 Huffman tables when the stream defined none.
///
/// Fills both id slots (luma → 0, chroma → 1) for DC and AC and marks all four
/// present (`htbl_entry = 0x0f`), so any `SOS` table selector resolves — exactly
/// MPP's `jpegd_setup_default_dht` behaviour.
fn setup_default_dht(info: &mut JpegInfo) {
    info.dc_tables[0] = DcHuffTable {
        bits: DEFAULT_DC_LUMA_BITS,
        vals: DEFAULT_DC_VALS,
    };
    info.dc_tables[1] = DcHuffTable {
        bits: DEFAULT_DC_CHROMA_BITS,
        vals: DEFAULT_DC_VALS,
    };
    info.ac_tables[0] = AcHuffTable {
        bits: DEFAULT_AC_LUMA_BITS,
        vals: DEFAULT_AC_LUMA_VALS,
    };
    info.ac_tables[1] = AcHuffTable {
        bits: DEFAULT_AC_CHROMA_BITS,
        vals: DEFAULT_AC_CHROMA_VALS,
    };
    info.htbl_entry = 0x0f;
}

/// Parse a baseline JPEG header from `data`.
///
/// Stops at the start-of-scan; returns the populated [`JpegInfo`] with
/// [`JpegInfo::strm_offset`] pointing at the first entropy-coded byte.
pub fn parse(data: &[u8]) -> Result<JpegInfo, ParseError> {
    if data.len() < 2 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(ParseError::MissingSoi);
    }

    let mut info = JpegInfo::ZERO;
    info.pkt_len = data.len() as u32;

    let mut pos = 2;
    let mut sof_seen = false;
    let mut dht_seen = false;
    loop {
        // A marker is 0xFF followed by a non-0xFF code; 0xFF fill bytes are skipped.
        if pos + 2 > data.len() {
            return Err(ParseError::Truncated);
        }
        if data[pos] != 0xFF {
            return Err(ParseError::BadSegment);
        }
        let mut m = pos + 1;
        while m < data.len() && data[m] == 0xFF {
            m += 1;
        }
        if m >= data.len() {
            return Err(ParseError::Truncated);
        }
        let marker = data[m];
        pos = m + 1;

        match marker {
            // Standalone markers (no length payload).
            0x01 | 0xD0..=0xD7 => continue,
            0xD9 => return Err(ParseError::MissingSos), // EOI before SOS
            0xC0 => {
                let (body, next) = read_segment(data, pos)?;
                parse_sof0(body, &mut info)?;
                sof_seen = true;
                pos = next;
            }
            // Any other SOF (extended/progressive/lossless) or arithmetic coding.
            0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCC | 0xCD | 0xCE
            | 0xCF => return Err(ParseError::NotBaseline),
            0xC4 => {
                let (body, next) = read_segment(data, pos)?;
                parse_dht(body, &mut info)?;
                dht_seen = true;
                pos = next;
            }
            0xDB => {
                let (body, next) = read_segment(data, pos)?;
                parse_dqt(body, &mut info)?;
                pos = next;
            }
            0xDD => {
                let (body, next) = read_segment(data, pos)?;
                if body.len() < 2 {
                    return Err(ParseError::BadSegment);
                }
                info.restart_interval = u16::from_be_bytes([body[0], body[1]]);
                pos = next;
            }
            0xDA => {
                if !sof_seen {
                    return Err(ParseError::MissingSof);
                }
                let (body, next) = read_segment(data, pos)?;
                parse_sos(body, &mut info)?;
                info.strm_offset = next as u32;
                // There must be entropy-coded data after the scan header.
                if info.strm_offset >= info.pkt_len {
                    return Err(ParseError::Truncated);
                }
                // A stream with no DHT (UVC / motion-JPEG) relies on the standard
                // tables; install them so the table buffer is well-formed.
                if !dht_seen {
                    setup_default_dht(&mut info);
                }
                return Ok(info);
            }
            // APPn, COM, JPG, DNL, and anything else with a length payload.
            _ => {
                let (_body, next) = read_segment(data, pos)?;
                pos = next;
            }
        }
    }
}

/// Read a length-prefixed marker segment body starting at `pos` (the length
/// field). Returns the body slice and the offset just past the segment.
fn read_segment(data: &[u8], pos: usize) -> Result<(&[u8], usize), ParseError> {
    if pos + 2 > data.len() {
        return Err(ParseError::Truncated);
    }
    let len = ((data[pos] as usize) << 8) | data[pos + 1] as usize;
    if len < 2 {
        return Err(ParseError::BadSegment);
    }
    let end = pos + len;
    if end > data.len() {
        return Err(ParseError::Truncated);
    }
    Ok((&data[pos + 2..end], end))
}

fn parse_dqt(body: &[u8], info: &mut JpegInfo) -> Result<(), ParseError> {
    let mut i = 0;
    while i < body.len() {
        let pq_tq = body[i];
        i += 1;
        let pq = pq_tq >> 4;
        let tq = (pq_tq & 0x0f) as usize;
        if tq >= NUM_QUANT_TABLES {
            return Err(ParseError::TableIdOutOfRange);
        }
        // Pq (precision) is 0 (8-bit) or 1 (16-bit) per T.81 Annex B.2.4.1; any
        // other value is malformed.
        if pq > 1 {
            return Err(ParseError::BadSegment);
        }
        info.qtbl_entry = info.qtbl_entry.saturating_add(1);
        if pq == 0 {
            if i + QUANT_LEN > body.len() {
                return Err(ParseError::BadSegment);
            }
            for k in 0..QUANT_LEN {
                info.quant_tables[tq][k] = body[i + k] as u16;
            }
            i += QUANT_LEN;
        } else {
            if i + 2 * QUANT_LEN > body.len() {
                return Err(ParseError::BadSegment);
            }
            for k in 0..QUANT_LEN {
                info.quant_tables[tq][k] =
                    u16::from_be_bytes([body[i + 2 * k], body[i + 2 * k + 1]]);
            }
            i += 2 * QUANT_LEN;
        }
    }
    Ok(())
}

fn parse_dht(body: &[u8], info: &mut JpegInfo) -> Result<(), ParseError> {
    let mut i = 0;
    while i < body.len() {
        let tc_th = body[i];
        i += 1;
        let tc = tc_th >> 4;
        let th = (tc_th & 0x0f) as usize;
        if th >= NUM_HUFF_TABLES {
            return Err(ParseError::TableIdOutOfRange);
        }
        if i + 16 > body.len() {
            return Err(ParseError::BadSegment);
        }
        let mut bits = [0u8; 16];
        let mut total = 0usize;
        for (k, slot) in bits.iter_mut().enumerate() {
            *slot = body[i + k];
            total += *slot as usize;
        }
        i += 16;
        if i + total > body.len() {
            return Err(ParseError::BadSegment);
        }
        match tc {
            0 => {
                if total > MAX_DC_VALS {
                    return Err(ParseError::BadSegment);
                }
                let mut t = DcHuffTable::ZERO;
                t.bits = bits;
                t.vals[..total].copy_from_slice(&body[i..i + total]);
                info.dc_tables[th] = t;
                info.htbl_entry |= 1 << (th * 2);
            }
            1 => {
                if total > MAX_AC_VALS {
                    return Err(ParseError::BadSegment);
                }
                let mut t = AcHuffTable::ZERO;
                t.bits = bits;
                t.vals[..total].copy_from_slice(&body[i..i + total]);
                info.ac_tables[th] = t;
                info.htbl_entry |= 1 << (th * 2 + 1);
            }
            _ => return Err(ParseError::BadSegment),
        }
        i += total;
    }
    Ok(())
}

fn parse_sof0(body: &[u8], info: &mut JpegInfo) -> Result<(), ParseError> {
    if body.len() < 6 {
        return Err(ParseError::BadSegment);
    }
    if body[0] != 8 {
        return Err(ParseError::UnsupportedPrecision);
    }
    info.height = u16::from_be_bytes([body[1], body[2]]);
    info.width = u16::from_be_bytes([body[3], body[4]]);
    // A baseline frame must carry non-zero dimensions (height 0 would require a
    // DNL marker, which this parser does not handle). Reject zero so it cannot
    // reach `build_reg_array`, where `width.wrapping_sub(1)` would program a
    // 65535-wide picture.
    if info.width == 0 || info.height == 0 {
        return Err(ParseError::BadSegment);
    }
    let nc = body[5] as usize;
    if nc == 0 || nc > MAX_COMPONENTS {
        return Err(ParseError::TooManyComponents);
    }
    if body.len() < 6 + nc * 3 {
        return Err(ParseError::BadSegment);
    }
    info.nb_components = nc as u8;
    let mut h_max = 1u8;
    let mut v_max = 1u8;
    for c in 0..nc {
        let off = 6 + c * 3;
        let hv = body[off + 1];
        let h = hv >> 4;
        let v = hv & 0x0f;
        let tq = body[off + 2];
        if tq as usize >= NUM_QUANT_TABLES {
            return Err(ParseError::TableIdOutOfRange);
        }
        if h == 0 || v == 0 {
            return Err(ParseError::UnsupportedSubsampling);
        }
        info.components[c] = Component {
            id: body[off],
            h,
            v,
            quant_index: tq,
            dc_index: 0,
            ac_index: 0,
        };
        h_max = h_max.max(h);
        v_max = v_max.max(v);
    }
    info.h_max = h_max;
    info.v_max = v_max;
    info.yuv_mode = derive_yuv_mode(info)?;

    // MCU-edge padding flags, matching the vendor `jpeg_judge_yuv_mode`.
    let (fill_right, fill_bottom) = match info.yuv_mode {
        YuvMode::Yuv420 => (false, false),
        YuvMode::Yuv422 => (false, needs_fill(info.height)),
        YuvMode::Yuv440 => (needs_fill(info.width), false),
        YuvMode::Yuv444 => (needs_fill(info.width), needs_fill(info.height)),
        YuvMode::Yuv411 => (false, needs_fill(info.height)),
        YuvMode::Yuv400 => (needs_fill(info.width), needs_fill(info.height)),
    };
    info.fill_right = fill_right;
    info.fill_bottom = fill_bottom;
    Ok(())
}

/// MCU padding is needed when the dimension's remainder mod 16 is in `1..=8`.
fn needs_fill(dim: u16) -> bool {
    let r = dim & 0xf;
    r != 0 && r <= 8
}

fn parse_sos(body: &[u8], info: &mut JpegInfo) -> Result<(), ParseError> {
    if body.is_empty() {
        return Err(ParseError::BadSegment);
    }
    let ns = body[0] as usize;
    if body.len() < 1 + ns * 2 + 3 {
        return Err(ParseError::BadSegment);
    }
    for s in 0..ns {
        let cs = body[1 + s * 2];
        let td_ta = body[1 + s * 2 + 1];
        let td = td_ta >> 4;
        let ta = td_ta & 0x0f;
        if td as usize >= NUM_HUFF_TABLES || ta as usize >= NUM_HUFF_TABLES {
            return Err(ParseError::TableIdOutOfRange);
        }
        for c in 0..info.nb_components as usize {
            if info.components[c].id == cs {
                info.components[c].dc_index = td;
                info.components[c].ac_index = ta;
            }
        }
    }
    Ok(())
}

/// Classify chroma subsampling from the component sampling factors.
fn derive_yuv_mode(info: &JpegInfo) -> Result<YuvMode, ParseError> {
    let c = &info.components;
    if info.nb_components == 1 {
        return Ok(YuvMode::Yuv400);
    }
    if info.nb_components != 3 {
        return Err(ParseError::UnsupportedSubsampling);
    }
    let chroma_unit = c[1].h == 1 && c[1].v == 1 && c[2].h == 1 && c[2].v == 1;
    if !chroma_unit {
        return Err(ParseError::UnsupportedSubsampling);
    }
    match (c[0].h, c[0].v) {
        (2, 2) => Ok(YuvMode::Yuv420),
        (2, 1) => Ok(YuvMode::Yuv422),
        (1, 2) => Ok(YuvMode::Yuv440),
        (1, 1) => Ok(YuvMode::Yuv444),
        (4, 1) => Ok(YuvMode::Yuv411),
        _ => Err(ParseError::UnsupportedSubsampling),
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use super::*;

    /// Standard Annex K luminance DC Huffman table.
    const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    const DC_LUMA_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    /// A small synthetic AC table (valid structure: sum(bits) == vals.len()).
    const AC_SMALL_BITS: [u8; 16] = [0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const AC_SMALL_VALS: [u8; 2] = [0x01, 0x02];

    struct CompSpec {
        id: u8,
        h: u8,
        v: u8,
        tq: u8,
        td: u8,
        ta: u8,
    }

    /// Build a baseline JPEG header + dummy scan + EOI. Returns (bytes, scan_offset).
    fn build_jpeg(
        sof_marker: u8,
        dims: (u16, u16),
        comps: &[CompSpec],
        quant: &[(u8, [u8; 64])],
        dc: &[(u8, [u8; 16], &[u8])],
        ac: &[(u8, [u8; 16], &[u8])],
        dri: Option<u16>,
    ) -> (Vec<u8>, u32) {
        let (width, height) = dims;
        let mut b = Vec::new();
        let seg = |b: &mut Vec<u8>, marker: u8, body: &[u8]| {
            let len = (body.len() + 2) as u16;
            b.extend_from_slice(&[0xFF, marker, (len >> 8) as u8, (len & 0xff) as u8]);
            b.extend_from_slice(body);
        };

        b.extend_from_slice(&[0xFF, 0xD8]); // SOI

        for (tq, table) in quant {
            let mut body = std::vec![*tq & 0x0f]; // Pq=0 (8-bit), Tq
            body.extend_from_slice(table);
            seg(&mut b, 0xDB, &body);
        }
        for (th, bits, vals) in dc {
            let mut body = std::vec![*th & 0x0f]; // Tc=0 (DC), Th
            body.extend_from_slice(bits);
            body.extend_from_slice(vals);
            seg(&mut b, 0xC4, &body);
        }
        for (th, bits, vals) in ac {
            let mut body = std::vec![0x10 | (*th & 0x0f)]; // Tc=1 (AC), Th
            body.extend_from_slice(bits);
            body.extend_from_slice(vals);
            seg(&mut b, 0xC4, &body);
        }
        if let Some(ri) = dri {
            seg(&mut b, 0xDD, &[(ri >> 8) as u8, (ri & 0xff) as u8]);
        }

        // SOF
        let mut sof = std::vec![
            8, // precision
            (height >> 8) as u8,
            (height & 0xff) as u8,
            (width >> 8) as u8,
            (width & 0xff) as u8,
            comps.len() as u8,
        ];
        for c in comps {
            sof.extend_from_slice(&[c.id, (c.h << 4) | (c.v & 0x0f), c.tq]);
        }
        seg(&mut b, sof_marker, &sof);

        // SOS
        let mut sos = std::vec![comps.len() as u8];
        for c in comps {
            sos.extend_from_slice(&[c.id, (c.td << 4) | (c.ta & 0x0f)]);
        }
        sos.extend_from_slice(&[0, 63, 0]); // Ss, Se, Ah/Al
        seg(&mut b, 0xDA, &sos);

        let scan_offset = b.len() as u32;
        b.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]); // dummy entropy data
        b.extend_from_slice(&[0xFF, 0xD9]); // EOI
        (b, scan_offset)
    }

    fn yuv420_comps() -> Vec<CompSpec> {
        std::vec![
            CompSpec {
                id: 1,
                h: 2,
                v: 2,
                tq: 0,
                td: 0,
                ta: 0
            },
            CompSpec {
                id: 2,
                h: 1,
                v: 1,
                tq: 1,
                td: 1,
                ta: 1
            },
            CompSpec {
                id: 3,
                h: 1,
                v: 1,
                tq: 1,
                td: 1,
                ta: 1
            },
        ]
    }

    fn qtables() -> Vec<(u8, [u8; 64])> {
        let mut q0 = [0u8; 64];
        let mut q1 = [0u8; 64];
        for i in 0..64 {
            q0[i] = (i + 1) as u8;
            q1[i] = (100 + i) as u8;
        }
        std::vec![(0, q0), (1, q1)]
    }

    fn parse_420() -> JpegInfo {
        let comps = yuv420_comps();
        let q = qtables();
        let dc: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, DC_LUMA_BITS, &DC_LUMA_VALS[..]),
            (1, DC_LUMA_BITS, &DC_LUMA_VALS[..])
        ];
        let ac: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, AC_SMALL_BITS, &AC_SMALL_VALS[..]),
            (1, AC_SMALL_BITS, &AC_SMALL_VALS[..])
        ];
        let (bytes, _off) = build_jpeg(0xC0, (64, 48), &comps, &q, &dc, &ac, None);
        parse(&bytes).expect("baseline 4:2:0 should parse")
    }

    #[test]
    fn parses_dimensions_and_component_count() {
        let info = parse_420();
        assert_eq!(info.width, 64);
        assert_eq!(info.height, 48);
        assert_eq!(info.nb_components, 3);
    }

    #[test]
    fn derives_yuv420_and_max_sampling() {
        let info = parse_420();
        assert_eq!(info.yuv_mode, YuvMode::Yuv420);
        assert_eq!(info.h_max, 2);
        assert_eq!(info.v_max, 2);
    }

    #[test]
    fn extracts_component_table_selectors() {
        let info = parse_420();
        assert_eq!(info.components[0].id, 1);
        assert_eq!((info.components[0].h, info.components[0].v), (2, 2));
        assert_eq!(info.components[0].quant_index, 0);
        assert_eq!(info.components[0].dc_index, 0);
        assert_eq!(info.components[0].ac_index, 0);
        assert_eq!(info.components[1].quant_index, 1);
        assert_eq!(info.components[1].dc_index, 1);
        assert_eq!(info.components[2].ac_index, 1);
    }

    #[test]
    fn extracts_quant_tables_in_stream_order() {
        let info = parse_420();
        // Quantization tables stay in JPEG zig-zag order (the table builder de-zig-zags).
        for i in 0..64 {
            assert_eq!(info.quant_tables[0][i], (i + 1) as u16);
            assert_eq!(info.quant_tables[1][i], (100 + i) as u16);
        }
    }

    #[test]
    fn extracts_huffman_tables() {
        let info = parse_420();
        assert_eq!(info.dc_tables[0].bits, DC_LUMA_BITS);
        assert_eq!(&info.dc_tables[0].vals[..], &DC_LUMA_VALS[..]);
        assert_eq!(info.ac_tables[0].bits, AC_SMALL_BITS);
        assert_eq!(info.ac_tables[0].vals[0], 0x01);
        assert_eq!(info.ac_tables[0].vals[1], 0x02);
    }

    #[test]
    fn installs_default_dht_when_absent() {
        // A JPEG with no DHT segment (typical UVC / motion-JPEG): the parser must
        // install the standard ITU-T T.81 Annex K.3 tables into both the luma and
        // chroma id slots and report all four tables present, matching MPP's
        // jpegd_setup_default_dht.
        let comps = yuv420_comps();
        let q = qtables();
        let no_dc: Vec<(u8, [u8; 16], &[u8])> = std::vec![];
        let no_ac: Vec<(u8, [u8; 16], &[u8])> = std::vec![];
        let (bytes, _off) = build_jpeg(0xC0, (64, 48), &comps, &q, &no_dc, &no_ac, None);
        let info = parse(&bytes).expect("DHT-less baseline 4:2:0 should parse with default tables");

        assert_eq!(
            info.dc_tables[0].bits,
            [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            &info.dc_tables[0].vals[..],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
        );
        assert_eq!(
            info.dc_tables[1].bits,
            [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            &info.dc_tables[1].vals[..],
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
        );
        assert_eq!(
            info.ac_tables[0].bits,
            [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125]
        );
        assert_eq!(
            info.ac_tables[1].bits,
            [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119]
        );
        // AC value arrays: spot-check the endpoints of each standard table.
        assert_eq!(info.ac_tables[0].vals[0], 0x01);
        assert_eq!(info.ac_tables[0].vals[161], 0xfa);
        assert_eq!(info.ac_tables[1].vals[0], 0x00);
        assert_eq!(info.ac_tables[1].vals[161], 0xfa);
        // The sum of each bits array equals the number of values (well-formed).
        assert_eq!(
            info.ac_tables[0]
                .bits
                .iter()
                .map(|&b| b as usize)
                .sum::<usize>(),
            162
        );
        assert_eq!(
            info.ac_tables[1]
                .bits
                .iter()
                .map(|&b| b as usize)
                .sum::<usize>(),
            162
        );
        // All four tables reported present, like MPP forcing htbl_entry = 0x0f.
        assert_eq!(info.htbl_entry, 0x0f);
    }

    #[test]
    fn keeps_stream_dht_when_present() {
        // When the stream carries its own DHT, the defaults must NOT overwrite it.
        let info = parse_420();
        assert_eq!(info.dc_tables[0].bits, DC_LUMA_BITS);
        assert_eq!(info.ac_tables[0].bits, AC_SMALL_BITS);
        assert_eq!(
            &info.ac_tables[0].vals[..AC_SMALL_VALS.len()],
            &AC_SMALL_VALS[..]
        );
    }

    #[test]
    fn computes_scan_offset_and_packet_length() {
        let comps = yuv420_comps();
        let q = qtables();
        let dc: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, DC_LUMA_BITS, &DC_LUMA_VALS[..]),
            (1, DC_LUMA_BITS, &DC_LUMA_VALS[..])
        ];
        let ac: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, AC_SMALL_BITS, &AC_SMALL_VALS[..]),
            (1, AC_SMALL_BITS, &AC_SMALL_VALS[..])
        ];
        let (bytes, off) = build_jpeg(0xC0, (64, 48), &comps, &q, &dc, &ac, None);
        let info = parse(&bytes).unwrap();
        assert_eq!(info.strm_offset, off);
        assert_eq!(info.pkt_len, bytes.len() as u32);

        // Truncate right at the scan offset: SOS is the final segment with no
        // entropy data, so the parser must reject it (would otherwise underflow
        // the stream-length computation).
        assert_eq!(parse(&bytes[..off as usize]), Err(ParseError::Truncated));
    }

    #[test]
    fn parses_restart_interval() {
        let comps = yuv420_comps();
        let q = qtables();
        let dc: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, DC_LUMA_BITS, &DC_LUMA_VALS[..]),
            (1, DC_LUMA_BITS, &DC_LUMA_VALS[..])
        ];
        let ac: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, AC_SMALL_BITS, &AC_SMALL_VALS[..]),
            (1, AC_SMALL_BITS, &AC_SMALL_VALS[..])
        ];
        let (bytes, _off) = build_jpeg(0xC0, (64, 48), &comps, &q, &dc, &ac, Some(8));
        let info = parse(&bytes).unwrap();
        assert_eq!(info.restart_interval, 8);
    }

    #[test]
    fn derives_yuv422() {
        let comps = std::vec![
            CompSpec {
                id: 1,
                h: 2,
                v: 1,
                tq: 0,
                td: 0,
                ta: 0
            },
            CompSpec {
                id: 2,
                h: 1,
                v: 1,
                tq: 1,
                td: 1,
                ta: 1
            },
            CompSpec {
                id: 3,
                h: 1,
                v: 1,
                tq: 1,
                td: 1,
                ta: 1
            },
        ];
        let q = qtables();
        let dc: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, DC_LUMA_BITS, &DC_LUMA_VALS[..]),
            (1, DC_LUMA_BITS, &DC_LUMA_VALS[..])
        ];
        let ac: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, AC_SMALL_BITS, &AC_SMALL_VALS[..]),
            (1, AC_SMALL_BITS, &AC_SMALL_VALS[..])
        ];
        let (bytes, _off) = build_jpeg(0xC0, (64, 48), &comps, &q, &dc, &ac, None);
        assert_eq!(parse(&bytes).unwrap().yuv_mode, YuvMode::Yuv422);
    }

    #[test]
    fn derives_grayscale_yuv400() {
        let comps = std::vec![CompSpec {
            id: 1,
            h: 1,
            v: 1,
            tq: 0,
            td: 0,
            ta: 0
        }];
        let q: Vec<(u8, [u8; 64])> = std::vec![(0, [1u8; 64])];
        let dc: Vec<(u8, [u8; 16], &[u8])> = std::vec![(0, DC_LUMA_BITS, &DC_LUMA_VALS[..])];
        let ac: Vec<(u8, [u8; 16], &[u8])> = std::vec![(0, AC_SMALL_BITS, &AC_SMALL_VALS[..])];
        let (bytes, _off) = build_jpeg(0xC0, (32, 32), &comps, &q, &dc, &ac, None);
        let info = parse(&bytes).unwrap();
        assert_eq!(info.nb_components, 1);
        assert_eq!(info.yuv_mode, YuvMode::Yuv400);
    }

    #[test]
    fn rejects_progressive() {
        let comps = yuv420_comps();
        let q = qtables();
        let dc: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, DC_LUMA_BITS, &DC_LUMA_VALS[..]),
            (1, DC_LUMA_BITS, &DC_LUMA_VALS[..])
        ];
        let ac: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, AC_SMALL_BITS, &AC_SMALL_VALS[..]),
            (1, AC_SMALL_BITS, &AC_SMALL_VALS[..])
        ];
        // 0xC2 = SOF2 (progressive).
        let (bytes, _off) = build_jpeg(0xC2, (64, 48), &comps, &q, &dc, &ac, None);
        assert_eq!(parse(&bytes), Err(ParseError::NotBaseline));
    }

    #[test]
    fn counts_quant_and_huffman_table_entries() {
        let info = parse_420();
        // Two DQT tables (luma + chroma).
        assert_eq!(info.qtbl_entry, 2);
        // DC0, AC0, DC1, AC1 -> bits 0,1,2,3 set.
        assert_eq!(info.htbl_entry, 0x0f);
    }

    #[test]
    fn grayscale_table_entries() {
        let comps = std::vec![CompSpec {
            id: 1,
            h: 1,
            v: 1,
            tq: 0,
            td: 0,
            ta: 0
        }];
        let q: Vec<(u8, [u8; 64])> = std::vec![(0, [1u8; 64])];
        let dc: Vec<(u8, [u8; 16], &[u8])> = std::vec![(0, DC_LUMA_BITS, &DC_LUMA_VALS[..])];
        let ac: Vec<(u8, [u8; 16], &[u8])> = std::vec![(0, AC_SMALL_BITS, &AC_SMALL_VALS[..])];
        let (bytes, _off) = build_jpeg(0xC0, (32, 32), &comps, &q, &dc, &ac, None);
        let info = parse(&bytes).unwrap();
        assert_eq!(info.qtbl_entry, 1);
        assert_eq!(info.htbl_entry, 0b11); // DC0 + AC0
    }

    #[test]
    fn yuv420_needs_no_fill_for_aligned_size() {
        // 64x48 is MCU-aligned for 4:2:0 (16x16 MCU); 4:2:0 never sets fill flags.
        let info = parse_420();
        assert!(!info.fill_right);
        assert!(!info.fill_bottom);
    }

    #[test]
    fn yuv444_sets_fill_for_unaligned_size() {
        // width=20 -> (20 & 0xf)=4 in 1..=8 -> fill_right; height=20 -> fill_bottom.
        let comps = std::vec![
            CompSpec {
                id: 1,
                h: 1,
                v: 1,
                tq: 0,
                td: 0,
                ta: 0
            },
            CompSpec {
                id: 2,
                h: 1,
                v: 1,
                tq: 1,
                td: 1,
                ta: 1
            },
            CompSpec {
                id: 3,
                h: 1,
                v: 1,
                tq: 1,
                td: 1,
                ta: 1
            },
        ];
        let q = qtables();
        let dc: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, DC_LUMA_BITS, &DC_LUMA_VALS[..]),
            (1, DC_LUMA_BITS, &DC_LUMA_VALS[..])
        ];
        let ac: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, AC_SMALL_BITS, &AC_SMALL_VALS[..]),
            (1, AC_SMALL_BITS, &AC_SMALL_VALS[..])
        ];
        let (bytes, _off) = build_jpeg(0xC0, (20, 20), &comps, &q, &dc, &ac, None);
        let info = parse(&bytes).unwrap();
        assert_eq!(info.yuv_mode, YuvMode::Yuv444);
        assert!(info.fill_right);
        assert!(info.fill_bottom);
    }

    #[test]
    fn rejects_missing_soi() {
        assert_eq!(parse(&[0x00, 0x01, 0x02]), Err(ParseError::MissingSoi));
    }

    #[test]
    fn rejects_truncated() {
        assert_eq!(parse(&[0xFF, 0xD8, 0xFF]), Err(ParseError::Truncated));
    }

    #[test]
    fn parses_generated_baseline_asset() {
        // Parse the full self-test stream that `crate::selftest` builds from source
        // (a 64x64 baseline 4:2:0 frame with the standard tables), end to end.
        let mut buf = [0u8; crate::SELFTEST_JPEG_CAPACITY];
        let n = crate::write_selftest_jpeg(&mut buf).expect("encode self-test JPEG");
        let info = parse(&buf[..n]).expect("baseline 4:2:0 JPEG should parse");
        assert_eq!((info.width, info.height), (64, 64));
        assert_eq!(info.nb_components, 3);
        assert_eq!(info.yuv_mode, YuvMode::Yuv420);
        assert_eq!(info.qtbl_entry, 2);
        assert_eq!(info.htbl_entry, 0x0f);
        assert!(info.strm_offset > 0 && (info.strm_offset as usize) < info.pkt_len as usize);
    }

    #[test]
    fn default_dht_assembles_into_table_buffer() {
        // The DHT-less (UVC / motion-JPEG) camera path: the parser installs the
        // standard Annex K.3 tables, and the command-layer table buffer must place
        // those exact DC/AC values into each component's value region. The min-code
        // assembly is covered byte-for-byte by command::tests against EXPECTED_TABLE;
        // this exercises build_table_buffer on the *default* tables specifically.
        let comps = yuv420_comps();
        let q = qtables();
        let none: Vec<(u8, [u8; 16], &[u8])> = std::vec![];
        let (bytes, _off) = build_jpeg(0xC0, (64, 48), &comps, &q, &none, &none, None);
        let info = parse(&bytes).expect("DHT-less baseline 4:2:0 should parse");
        assert_eq!(info.htbl_entry, 0x0f);

        let mut out = [0u8; crate::command::TABLE_SIZE];
        crate::command::build_table_buffer(&info, &mut out);

        // Value-table region: VALUE_TBL_OFFSET=704, 192 bytes per component, DC
        // values at +0..12 and AC values at +16..16+162. Component 0 uses the luma
        // slot (id 0); components 1 and 2 use the chroma slot (id 1).
        let slots = [
            (&info.dc_tables[0], &info.ac_tables[0]),
            (&info.dc_tables[1], &info.ac_tables[1]),
            (&info.dc_tables[1], &info.ac_tables[1]),
        ];
        for (k, (dc, ac)) in slots.iter().enumerate() {
            let vbase = crate::command::VALUE_TBL_OFFSET + k * 192;
            assert_eq!(
                &out[vbase..vbase + 12],
                &dc.vals[..],
                "component {k} DC vals"
            );
            assert_eq!(
                &out[vbase + 16..vbase + 16 + 162],
                &ac.vals[..],
                "component {k} AC vals"
            );
        }
    }

    #[test]
    fn rejects_zero_dimensions() {
        // A baseline frame with a zero dimension is malformed (no DNL handling).
        let comps = yuv420_comps();
        let q = qtables();
        let dc: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, DC_LUMA_BITS, &DC_LUMA_VALS[..]),
            (1, DC_LUMA_BITS, &DC_LUMA_VALS[..])
        ];
        let ac: Vec<(u8, [u8; 16], &[u8])> = std::vec![
            (0, AC_SMALL_BITS, &AC_SMALL_VALS[..]),
            (1, AC_SMALL_BITS, &AC_SMALL_VALS[..])
        ];
        let (bytes, _off) = build_jpeg(0xC0, (0, 48), &comps, &q, &dc, &ac, None);
        assert_eq!(parse(&bytes), Err(ParseError::BadSegment));
    }

    #[test]
    fn rejects_bad_quant_precision() {
        // Pq must be 0 (8-bit) or 1 (16-bit); a DQT with Pq=2 is malformed.
        let mut info = JpegInfo::zeroed();
        assert_eq!(parse_dqt(&[0x20], &mut info), Err(ParseError::BadSegment));
    }
}

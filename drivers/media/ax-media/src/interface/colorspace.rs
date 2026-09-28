/// 色彩空间（Colorspace）。
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Colorspace(pub u32);

impl Colorspace {
    pub const DEFAULT: Self = Self(0); // 默认色彩空间（由驱动自行决定）。
    pub const SMPTE170M: Self = Self(1); // SMPTE 170M：广播电视 NTSC/PAL 标清（SDTV）。
    pub const SMPTE240M: Self = Self(2); // SMPTE 240M：已废弃的高清（HDTV）。
    pub const REC709: Self = Self(3); // Rec.709：高清（HDTV）。
    pub const SYSTEM470_M: Self = Self(5); // NTSC 1953 色彩空间。
    pub const SYSTEM470_BG: Self = Self(6); // EBU Tech 3213 PAL/SECAM。
    pub const JPEG: Self = Self(7); // 动态 JPEG（Motion-JPEG）。
    pub const SRGB: Self = Self(8); // sRGB。
    pub const OPRGB: Self = Self(9); // opRGB。
    pub const BT2020: Self = Self(10); // BT.2020，超高清（UHDTV）。
    pub const RAW: Self = Self(11); // 未经处理的原始图像。
    pub const DCI_P3: Self = Self(12); // DCI-P3，影院投影机。
}

/// 传输函数。
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct XferFunc(pub u32);

impl XferFunc {
    pub const DEFAULT: Self = Self(0);
    pub const REC709: Self = Self(1);
    pub const SRGB: Self = Self(2);
    pub const OPRGB: Self = Self(3);
    pub const SMPTE240M: Self = Self(4);
    /// 不使用任何传输函数（xfer func）。
    pub const NONE: Self = Self(5);
    pub const DCI_P3: Self = Self(6);
    pub const SMPTE2084: Self = Self(7);
}

/// Y'CbCr 编码。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum YcbcrEncoding {
    Default        = 0,
    Rec601         = 1, // ITU-R 601 —— 标清（SDTV）。
    Rec709         = 2, // Rec. 709 —— 高清（HDTV）。
    Xv601          = 3, // ITU-R 601/EN 61966-2-4 扩展色域 —— 标清（SDTV）。
    Xv709          = 4, // Rec. 709/EN 61966-2-4 扩展色域 —— 高清（HDTV）。
    Bt2020         = 6, // BT.2020 非常亮度（Non-constant Luminance）Y'CbCr。
    Bt2020ConstLum = 7, // BT.2020 恒定亮度（Constant Luminance）Y'CbcCrc。
}

/// HSV 编码。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HsvEncoding {
    Hue180 = 128, // 色相映射到 0 - 179
    Hue256 = 129, // 色相映射到 0-255。
}

/// 量化范围。
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Quantization(pub u32);

impl Quantization {
    pub const DEFAULT: Self = Self(0);
    pub const FULL_RANGE: Self = Self(1);
    pub const LIM_RANGE: Self = Self(2);
}

/// 色彩空间（Colorspace）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Colorspace {
    Default     = 0,  // 默认色彩空间（由驱动自行决定）。
    Smpte170m   = 1,  // SMPTE 170M：广播电视 NTSC/PAL 标清（SDTV）。
    Smpte240m   = 2,  // SMPTE 240M：已废弃的高清（HDTV）。
    Rec709      = 3,  // Rec.709：高清（HDTV）。
    System470M  = 5,  // NTSC 1953 色彩空间。
    System470Bg = 6,  // EBU Tech 3213 PAL/SECAM。
    Jpeg        = 7,  // 动态 JPEG（Motion-JPEG）。
    Srgb        = 8,  // sRGB。
    Oprgb       = 9,  // opRGB。
    Bt2020      = 10, // BT.2020，超高清（UHDTV）。
    Raw         = 11, // 未经处理的原始图像。
    DciP3       = 12, // DCI-P3，影院投影机。
}

/// 传输函数。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XferFunc {
    Default   = 0,
    Rec709    = 1,
    Srgb      = 2,
    Oprgb     = 3,
    Smpte240m = 4,
    /// 不使用任何传输函数（xfer func）。
    None      = 5,
    DciP3     = 6,
    Smpte2084 = 7,
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
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quantization {
    Default   = 0,
    FullRange = 1,
    LimRange  = 2,
}

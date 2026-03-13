use core::fmt::{Display, Formatter, LowerHex, Result, UpperHex};

/// Register type
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RegistersType {
    /// System register NONE
    NONE = 0x0,
    /// System register W0
    W0 = 0x1,
    /// System register W1
    W1 = 0x2,
    /// System register W2
    W2 = 0x3,
    /// System register W3
    W3 = 0x4,
    /// System register W4
    W4 = 0x5,
    /// System register W5
    W5 = 0x6,
    /// System register W6
    W6 = 0x7,
    /// System register W7
    W7 = 0x8,
    /// System register W8
    W8 = 0x9,
    /// System register W9
    W9 = 0xa,
    /// System register W10
    W10 = 0xb,
    /// System register W11
    W11 = 0xc,
    /// System register W12
    W12 = 0xd,
    /// System register W13
    W13 = 0xe,
    /// System register W14
    W14 = 0xf,
    /// System register W15
    W15 = 0x10,
    /// System register W16
    W16 = 0x11,
    /// System register W17
    W17 = 0x12,
    /// System register W18
    W18 = 0x13,
    /// System register W19
    W19 = 0x14,
    /// System register W20
    W20 = 0x15,
    /// System register W21
    W21 = 0x16,
    /// System register W22
    W22 = 0x17,
    /// System register W23
    W23 = 0x18,
    /// System register W24
    W24 = 0x19,
    /// System register W25
    W25 = 0x1a,
    /// System register W26
    W26 = 0x1b,
    /// System register W27
    W27 = 0x1c,
    /// System register W28
    W28 = 0x1d,
    /// System register W29
    W29 = 0x1e,
    /// System register W30
    W30 = 0x1f,
    /// System register WZR
    WZR = 0x20,
    /// System register WSP
    WSP = 0x21,
    /// System register X0
    X0 = 0x22,
    /// System register X1
    X1 = 0x23,
    /// System register X2
    X2 = 0x24,
    /// System register X3
    X3 = 0x25,
    /// System register X4
    X4 = 0x26,
    /// System register X5
    X5 = 0x27,
    /// System register X6
    X6 = 0x28,
    /// System register X7
    X7 = 0x29,
    /// System register X8
    X8 = 0x2a,
    /// System register X9
    X9 = 0x2b,
    /// System register X10
    X10 = 0x2c,
    /// System register X11
    X11 = 0x2d,
    /// System register X12
    X12 = 0x2e,
    /// System register X13
    X13 = 0x2f,
    /// System register X14
    X14 = 0x30,
    /// System register X15
    X15 = 0x31,
    /// System register X16
    X16 = 0x32,
    /// System register X17
    X17 = 0x33,
    /// System register X18
    X18 = 0x34,
    /// System register X19
    X19 = 0x35,
    /// System register X20
    X20 = 0x36,
    /// System register X21
    X21 = 0x37,
    /// System register X22
    X22 = 0x38,
    /// System register X23
    X23 = 0x39,
    /// System register X24
    X24 = 0x3a,
    /// System register X25
    X25 = 0x3b,
    /// System register X26
    X26 = 0x3c,
    /// System register X27
    X27 = 0x3d,
    /// System register X28
    X28 = 0x3e,
    /// System register X29
    X29 = 0x3f,
    /// System register X30
    X30 = 0x40,
    /// System register XZR
    XZR = 0x41,
    /// System register SP
    SP = 0x42,
    /// System register V0
    V0 = 0x43,
    /// System register V1
    V1 = 0x44,
    /// System register V2
    V2 = 0x45,
    /// System register V3
    V3 = 0x46,
    /// System register V4
    V4 = 0x47,
    /// System register V5
    V5 = 0x48,
    /// System register V6
    V6 = 0x49,
    /// System register V7
    V7 = 0x4a,
    /// System register V8
    V8 = 0x4b,
    /// System register V9
    V9 = 0x4c,
    /// System register V10
    V10 = 0x4d,
    /// System register V11
    V11 = 0x4e,
    /// System register V12
    V12 = 0x4f,
    /// System register V13
    V13 = 0x50,
    /// System register V14
    V14 = 0x51,
    /// System register V15
    V15 = 0x52,
    /// System register V16
    V16 = 0x53,
    /// System register V17
    V17 = 0x54,
    /// System register V18
    V18 = 0x55,
    /// System register V19
    V19 = 0x56,
    /// System register V20
    V20 = 0x57,
    /// System register V21
    V21 = 0x58,
    /// System register V22
    V22 = 0x59,
    /// System register V23
    V23 = 0x5a,
    /// System register V24
    V24 = 0x5b,
    /// System register V25
    V25 = 0x5c,
    /// System register V26
    V26 = 0x5d,
    /// System register V27
    V27 = 0x5e,
    /// System register V28
    V28 = 0x5f,
    /// System register V29
    V29 = 0x60,
    /// System register V30
    V30 = 0x61,
    /// System register V31
    V31 = 0x62,
    /// System register B0
    B0 = 0x63,
    /// System register B1
    B1 = 0x64,
    /// System register B2
    B2 = 0x65,
    /// System register B3
    B3 = 0x66,
    /// System register B4
    B4 = 0x67,
    /// System register B5
    B5 = 0x68,
    /// System register B6
    B6 = 0x69,
    /// System register B7
    B7 = 0x6a,
    /// System register B8
    B8 = 0x6b,
    /// System register B9
    B9 = 0x6c,
    /// System register B10
    B10 = 0x6d,
    /// System register B11
    B11 = 0x6e,
    /// System register B12
    B12 = 0x6f,
    /// System register B13
    B13 = 0x70,
    /// System register B14
    B14 = 0x71,
    /// System register B15
    B15 = 0x72,
    /// System register B16
    B16 = 0x73,
    /// System register B17
    B17 = 0x74,
    /// System register B18
    B18 = 0x75,
    /// System register B19
    B19 = 0x76,
    /// System register B20
    B20 = 0x77,
    /// System register B21
    B21 = 0x78,
    /// System register B22
    B22 = 0x79,
    /// System register B23
    B23 = 0x7a,
    /// System register B24
    B24 = 0x7b,
    /// System register B25
    B25 = 0x7c,
    /// System register B26
    B26 = 0x7d,
    /// System register B27
    B27 = 0x7e,
    /// System register B28
    B28 = 0x7f,
    /// System register B29
    B29 = 0x80,
    /// System register B30
    B30 = 0x81,
    /// System register B31
    B31 = 0x82,
    /// System register H0
    H0 = 0x83,
    /// System register H1
    H1 = 0x84,
    /// System register H2
    H2 = 0x85,
    /// System register H3
    H3 = 0x86,
    /// System register H4
    H4 = 0x87,
    /// System register H5
    H5 = 0x88,
    /// System register H6
    H6 = 0x89,
    /// System register H7
    H7 = 0x8a,
    /// System register H8
    H8 = 0x8b,
    /// System register H9
    H9 = 0x8c,
    /// System register H10
    H10 = 0x8d,
    /// System register H11
    H11 = 0x8e,
    /// System register H12
    H12 = 0x8f,
    /// System register H13
    H13 = 0x90,
    /// System register H14
    H14 = 0x91,
    /// System register H15
    H15 = 0x92,
    /// System register H16
    H16 = 0x93,
    /// System register H17
    H17 = 0x94,
    /// System register H18
    H18 = 0x95,
    /// System register H19
    H19 = 0x96,
    /// System register H20
    H20 = 0x97,
    /// System register H21
    H21 = 0x98,
    /// System register H22
    H22 = 0x99,
    /// System register H23
    H23 = 0x9a,
    /// System register H24
    H24 = 0x9b,
    /// System register H25
    H25 = 0x9c,
    /// System register H26
    H26 = 0x9d,
    /// System register H27
    H27 = 0x9e,
    /// System register H28
    H28 = 0x9f,
    /// System register H29
    H29 = 0xa0,
    /// System register H30
    H30 = 0xa1,
    /// System register H31
    H31 = 0xa2,
    /// System register S0
    S0 = 0xa3,
    /// System register S1
    S1 = 0xa4,
    /// System register S2
    S2 = 0xa5,
    /// System register S3
    S3 = 0xa6,
    /// System register S4
    S4 = 0xa7,
    /// System register S5
    S5 = 0xa8,
    /// System register S6
    S6 = 0xa9,
    /// System register S7
    S7 = 0xaa,
    /// System register S8
    S8 = 0xab,
    /// System register S9
    S9 = 0xac,
    /// System register S10
    S10 = 0xad,
    /// System register S11
    S11 = 0xae,
    /// System register S12
    S12 = 0xaf,
    /// System register S13
    S13 = 0xb0,
    /// System register S14
    S14 = 0xb1,
    /// System register S15
    S15 = 0xb2,
    /// System register S16
    S16 = 0xb3,
    /// System register S17
    S17 = 0xb4,
    /// System register S18
    S18 = 0xb5,
    /// System register S19
    S19 = 0xb6,
    /// System register S20
    S20 = 0xb7,
    /// System register S21
    S21 = 0xb8,
    /// System register S22
    S22 = 0xb9,
    /// System register S23
    S23 = 0xba,
    /// System register S24
    S24 = 0xbb,
    /// System register S25
    S25 = 0xbc,
    /// System register S26
    S26 = 0xbd,
    /// System register S27
    S27 = 0xbe,
    /// System register S28
    S28 = 0xbf,
    /// System register S29
    S29 = 0xc0,
    /// System register S30
    S30 = 0xc1,
    /// System register S31
    S31 = 0xc2,
    /// System register D0
    D0 = 0xc3,
    /// System register D1
    D1 = 0xc4,
    /// System register D2
    D2 = 0xc5,
    /// System register D3
    D3 = 0xc6,
    /// System register D4
    D4 = 0xc7,
    /// System register D5
    D5 = 0xc8,
    /// System register D6
    D6 = 0xc9,
    /// System register D7
    D7 = 0xca,
    /// System register D8
    D8 = 0xcb,
    /// System register D9
    D9 = 0xcc,
    /// System register D10
    D10 = 0xcd,
    /// System register D11
    D11 = 0xce,
    /// System register D12
    D12 = 0xcf,
    /// System register D13
    D13 = 0xd0,
    /// System register D14
    D14 = 0xd1,
    /// System register D15
    D15 = 0xd2,
    /// System register D16
    D16 = 0xd3,
    /// System register D17
    D17 = 0xd4,
    /// System register D18
    D18 = 0xd5,
    /// System register D19
    D19 = 0xd6,
    /// System register D20
    D20 = 0xd7,
    /// System register D21
    D21 = 0xd8,
    /// System register D22
    D22 = 0xd9,
    /// System register D23
    D23 = 0xda,
    /// System register D24
    D24 = 0xdb,
    /// System register D25
    D25 = 0xdc,
    /// System register D26
    D26 = 0xdd,
    /// System register D27
    D27 = 0xde,
    /// System register D28
    D28 = 0xdf,
    /// System register D29
    D29 = 0xe0,
    /// System register D30
    D30 = 0xe1,
    /// System register D31
    D31 = 0xe2,
    /// System register Q0
    Q0 = 0xe3,
    /// System register Q1
    Q1 = 0xe4,
    /// System register Q2
    Q2 = 0xe5,
    /// System register Q3
    Q3 = 0xe6,
    /// System register Q4
    Q4 = 0xe7,
    /// System register Q5
    Q5 = 0xe8,
    /// System register Q6
    Q6 = 0xe9,
    /// System register Q7
    Q7 = 0xea,
    /// System register Q8
    Q8 = 0xeb,
    /// System register Q9
    Q9 = 0xec,
    /// System register Q10
    Q10 = 0xed,
    /// System register Q11
    Q11 = 0xee,
    /// System register Q12
    Q12 = 0xef,
    /// System register Q13
    Q13 = 0xf0,
    /// System register Q14
    Q14 = 0xf1,
    /// System register Q15
    Q15 = 0xf2,
    /// System register Q16
    Q16 = 0xf3,
    /// System register Q17
    Q17 = 0xf4,
    /// System register Q18
    Q18 = 0xf5,
    /// System register Q19
    Q19 = 0xf6,
    /// System register Q20
    Q20 = 0xf7,
    /// System register Q21
    Q21 = 0xf8,
    /// System register Q22
    Q22 = 0xf9,
    /// System register Q23
    Q23 = 0xfa,
    /// System register Q24
    Q24 = 0xfb,
    /// System register Q25
    Q25 = 0xfc,
    /// System register Q26
    Q26 = 0xfd,
    /// System register Q27
    Q27 = 0xfe,
    /// System register Q28
    Q28 = 0xff,
    /// System register Q29
    Q29 = 0x100,
    /// System register Q30
    Q30 = 0x101,
    /// System register Q31
    Q31 = 0x102,
    /// System register V0_B0
    V0_B0 = 0x103,
    /// System register V0_B1
    V0_B1 = 0x104,
    /// System register V0_B2
    V0_B2 = 0x105,
    /// System register V0_B3
    V0_B3 = 0x106,
    /// System register V0_B4
    V0_B4 = 0x107,
    /// System register V0_B5
    V0_B5 = 0x108,
    /// System register V0_B6
    V0_B6 = 0x109,
    /// System register V0_B7
    V0_B7 = 0x10a,
    /// System register V0_B8
    V0_B8 = 0x10b,
    /// System register V0_B9
    V0_B9 = 0x10c,
    /// System register V0_B10
    V0_B10 = 0x10d,
    /// System register V0_B11
    V0_B11 = 0x10e,
    /// System register V0_B12
    V0_B12 = 0x10f,
    /// System register V0_B13
    V0_B13 = 0x110,
    /// System register V0_B14
    V0_B14 = 0x111,
    /// System register V0_B15
    V0_B15 = 0x112,
    /// System register V1_B0
    V1_B0 = 0x113,
    /// System register V1_B1
    V1_B1 = 0x114,
    /// System register V1_B2
    V1_B2 = 0x115,
    /// System register V1_B3
    V1_B3 = 0x116,
    /// System register V1_B4
    V1_B4 = 0x117,
    /// System register V1_B5
    V1_B5 = 0x118,
    /// System register V1_B6
    V1_B6 = 0x119,
    /// System register V1_B7
    V1_B7 = 0x11a,
    /// System register V1_B8
    V1_B8 = 0x11b,
    /// System register V1_B9
    V1_B9 = 0x11c,
    /// System register V1_B10
    V1_B10 = 0x11d,
    /// System register V1_B11
    V1_B11 = 0x11e,
    /// System register V1_B12
    V1_B12 = 0x11f,
    /// System register V1_B13
    V1_B13 = 0x120,
    /// System register V1_B14
    V1_B14 = 0x121,
    /// System register V1_B15
    V1_B15 = 0x122,
    /// System register V2_B0
    V2_B0 = 0x123,
    /// System register V2_B1
    V2_B1 = 0x124,
    /// System register V2_B2
    V2_B2 = 0x125,
    /// System register V2_B3
    V2_B3 = 0x126,
    /// System register V2_B4
    V2_B4 = 0x127,
    /// System register V2_B5
    V2_B5 = 0x128,
    /// System register V2_B6
    V2_B6 = 0x129,
    /// System register V2_B7
    V2_B7 = 0x12a,
    /// System register V2_B8
    V2_B8 = 0x12b,
    /// System register V2_B9
    V2_B9 = 0x12c,
    /// System register V2_B10
    V2_B10 = 0x12d,
    /// System register V2_B11
    V2_B11 = 0x12e,
    /// System register V2_B12
    V2_B12 = 0x12f,
    /// System register V2_B13
    V2_B13 = 0x130,
    /// System register V2_B14
    V2_B14 = 0x131,
    /// System register V2_B15
    V2_B15 = 0x132,
    /// System register V3_B0
    V3_B0 = 0x133,
    /// System register V3_B1
    V3_B1 = 0x134,
    /// System register V3_B2
    V3_B2 = 0x135,
    /// System register V3_B3
    V3_B3 = 0x136,
    /// System register V3_B4
    V3_B4 = 0x137,
    /// System register V3_B5
    V3_B5 = 0x138,
    /// System register V3_B6
    V3_B6 = 0x139,
    /// System register V3_B7
    V3_B7 = 0x13a,
    /// System register V3_B8
    V3_B8 = 0x13b,
    /// System register V3_B9
    V3_B9 = 0x13c,
    /// System register V3_B10
    V3_B10 = 0x13d,
    /// System register V3_B11
    V3_B11 = 0x13e,
    /// System register V3_B12
    V3_B12 = 0x13f,
    /// System register V3_B13
    V3_B13 = 0x140,
    /// System register V3_B14
    V3_B14 = 0x141,
    /// System register V3_B15
    V3_B15 = 0x142,
    /// System register V4_B0
    V4_B0 = 0x143,
    /// System register V4_B1
    V4_B1 = 0x144,
    /// System register V4_B2
    V4_B2 = 0x145,
    /// System register V4_B3
    V4_B3 = 0x146,
    /// System register V4_B4
    V4_B4 = 0x147,
    /// System register V4_B5
    V4_B5 = 0x148,
    /// System register V4_B6
    V4_B6 = 0x149,
    /// System register V4_B7
    V4_B7 = 0x14a,
    /// System register V4_B8
    V4_B8 = 0x14b,
    /// System register V4_B9
    V4_B9 = 0x14c,
    /// System register V4_B10
    V4_B10 = 0x14d,
    /// System register V4_B11
    V4_B11 = 0x14e,
    /// System register V4_B12
    V4_B12 = 0x14f,
    /// System register V4_B13
    V4_B13 = 0x150,
    /// System register V4_B14
    V4_B14 = 0x151,
    /// System register V4_B15
    V4_B15 = 0x152,
    /// System register V5_B0
    V5_B0 = 0x153,
    /// System register V5_B1
    V5_B1 = 0x154,
    /// System register V5_B2
    V5_B2 = 0x155,
    /// System register V5_B3
    V5_B3 = 0x156,
    /// System register V5_B4
    V5_B4 = 0x157,
    /// System register V5_B5
    V5_B5 = 0x158,
    /// System register V5_B6
    V5_B6 = 0x159,
    /// System register V5_B7
    V5_B7 = 0x15a,
    /// System register V5_B8
    V5_B8 = 0x15b,
    /// System register V5_B9
    V5_B9 = 0x15c,
    /// System register V5_B10
    V5_B10 = 0x15d,
    /// System register V5_B11
    V5_B11 = 0x15e,
    /// System register V5_B12
    V5_B12 = 0x15f,
    /// System register V5_B13
    V5_B13 = 0x160,
    /// System register V5_B14
    V5_B14 = 0x161,
    /// System register V5_B15
    V5_B15 = 0x162,
    /// System register V6_B0
    V6_B0 = 0x163,
    /// System register V6_B1
    V6_B1 = 0x164,
    /// System register V6_B2
    V6_B2 = 0x165,
    /// System register V6_B3
    V6_B3 = 0x166,
    /// System register V6_B4
    V6_B4 = 0x167,
    /// System register V6_B5
    V6_B5 = 0x168,
    /// System register V6_B6
    V6_B6 = 0x169,
    /// System register V6_B7
    V6_B7 = 0x16a,
    /// System register V6_B8
    V6_B8 = 0x16b,
    /// System register V6_B9
    V6_B9 = 0x16c,
    /// System register V6_B10
    V6_B10 = 0x16d,
    /// System register V6_B11
    V6_B11 = 0x16e,
    /// System register V6_B12
    V6_B12 = 0x16f,
    /// System register V6_B13
    V6_B13 = 0x170,
    /// System register V6_B14
    V6_B14 = 0x171,
    /// System register V6_B15
    V6_B15 = 0x172,
    /// System register V7_B0
    V7_B0 = 0x173,
    /// System register V7_B1
    V7_B1 = 0x174,
    /// System register V7_B2
    V7_B2 = 0x175,
    /// System register V7_B3
    V7_B3 = 0x176,
    /// System register V7_B4
    V7_B4 = 0x177,
    /// System register V7_B5
    V7_B5 = 0x178,
    /// System register V7_B6
    V7_B6 = 0x179,
    /// System register V7_B7
    V7_B7 = 0x17a,
    /// System register V7_B8
    V7_B8 = 0x17b,
    /// System register V7_B9
    V7_B9 = 0x17c,
    /// System register V7_B10
    V7_B10 = 0x17d,
    /// System register V7_B11
    V7_B11 = 0x17e,
    /// System register V7_B12
    V7_B12 = 0x17f,
    /// System register V7_B13
    V7_B13 = 0x180,
    /// System register V7_B14
    V7_B14 = 0x181,
    /// System register V7_B15
    V7_B15 = 0x182,
    /// System register V8_B0
    V8_B0 = 0x183,
    /// System register V8_B1
    V8_B1 = 0x184,
    /// System register V8_B2
    V8_B2 = 0x185,
    /// System register V8_B3
    V8_B3 = 0x186,
    /// System register V8_B4
    V8_B4 = 0x187,
    /// System register V8_B5
    V8_B5 = 0x188,
    /// System register V8_B6
    V8_B6 = 0x189,
    /// System register V8_B7
    V8_B7 = 0x18a,
    /// System register V8_B8
    V8_B8 = 0x18b,
    /// System register V8_B9
    V8_B9 = 0x18c,
    /// System register V8_B10
    V8_B10 = 0x18d,
    /// System register V8_B11
    V8_B11 = 0x18e,
    /// System register V8_B12
    V8_B12 = 0x18f,
    /// System register V8_B13
    V8_B13 = 0x190,
    /// System register V8_B14
    V8_B14 = 0x191,
    /// System register V8_B15
    V8_B15 = 0x192,
    /// System register V9_B0
    V9_B0 = 0x193,
    /// System register V9_B1
    V9_B1 = 0x194,
    /// System register V9_B2
    V9_B2 = 0x195,
    /// System register V9_B3
    V9_B3 = 0x196,
    /// System register V9_B4
    V9_B4 = 0x197,
    /// System register V9_B5
    V9_B5 = 0x198,
    /// System register V9_B6
    V9_B6 = 0x199,
    /// System register V9_B7
    V9_B7 = 0x19a,
    /// System register V9_B8
    V9_B8 = 0x19b,
    /// System register V9_B9
    V9_B9 = 0x19c,
    /// System register V9_B10
    V9_B10 = 0x19d,
    /// System register V9_B11
    V9_B11 = 0x19e,
    /// System register V9_B12
    V9_B12 = 0x19f,
    /// System register V9_B13
    V9_B13 = 0x1a0,
    /// System register V9_B14
    V9_B14 = 0x1a1,
    /// System register V9_B15
    V9_B15 = 0x1a2,
    /// System register V10_B0
    V10_B0 = 0x1a3,
    /// System register V10_B1
    V10_B1 = 0x1a4,
    /// System register V10_B2
    V10_B2 = 0x1a5,
    /// System register V10_B3
    V10_B3 = 0x1a6,
    /// System register V10_B4
    V10_B4 = 0x1a7,
    /// System register V10_B5
    V10_B5 = 0x1a8,
    /// System register V10_B6
    V10_B6 = 0x1a9,
    /// System register V10_B7
    V10_B7 = 0x1aa,
    /// System register V10_B8
    V10_B8 = 0x1ab,
    /// System register V10_B9
    V10_B9 = 0x1ac,
    /// System register V10_B10
    V10_B10 = 0x1ad,
    /// System register V10_B11
    V10_B11 = 0x1ae,
    /// System register V10_B12
    V10_B12 = 0x1af,
    /// System register V10_B13
    V10_B13 = 0x1b0,
    /// System register V10_B14
    V10_B14 = 0x1b1,
    /// System register V10_B15
    V10_B15 = 0x1b2,
    /// System register V11_B0
    V11_B0 = 0x1b3,
    /// System register V11_B1
    V11_B1 = 0x1b4,
    /// System register V11_B2
    V11_B2 = 0x1b5,
    /// System register V11_B3
    V11_B3 = 0x1b6,
    /// System register V11_B4
    V11_B4 = 0x1b7,
    /// System register V11_B5
    V11_B5 = 0x1b8,
    /// System register V11_B6
    V11_B6 = 0x1b9,
    /// System register V11_B7
    V11_B7 = 0x1ba,
    /// System register V11_B8
    V11_B8 = 0x1bb,
    /// System register V11_B9
    V11_B9 = 0x1bc,
    /// System register V11_B10
    V11_B10 = 0x1bd,
    /// System register V11_B11
    V11_B11 = 0x1be,
    /// System register V11_B12
    V11_B12 = 0x1bf,
    /// System register V11_B13
    V11_B13 = 0x1c0,
    /// System register V11_B14
    V11_B14 = 0x1c1,
    /// System register V11_B15
    V11_B15 = 0x1c2,
    /// System register V12_B0
    V12_B0 = 0x1c3,
    /// System register V12_B1
    V12_B1 = 0x1c4,
    /// System register V12_B2
    V12_B2 = 0x1c5,
    /// System register V12_B3
    V12_B3 = 0x1c6,
    /// System register V12_B4
    V12_B4 = 0x1c7,
    /// System register V12_B5
    V12_B5 = 0x1c8,
    /// System register V12_B6
    V12_B6 = 0x1c9,
    /// System register V12_B7
    V12_B7 = 0x1ca,
    /// System register V12_B8
    V12_B8 = 0x1cb,
    /// System register V12_B9
    V12_B9 = 0x1cc,
    /// System register V12_B10
    V12_B10 = 0x1cd,
    /// System register V12_B11
    V12_B11 = 0x1ce,
    /// System register V12_B12
    V12_B12 = 0x1cf,
    /// System register V12_B13
    V12_B13 = 0x1d0,
    /// System register V12_B14
    V12_B14 = 0x1d1,
    /// System register V12_B15
    V12_B15 = 0x1d2,
    /// System register V13_B0
    V13_B0 = 0x1d3,
    /// System register V13_B1
    V13_B1 = 0x1d4,
    /// System register V13_B2
    V13_B2 = 0x1d5,
    /// System register V13_B3
    V13_B3 = 0x1d6,
    /// System register V13_B4
    V13_B4 = 0x1d7,
    /// System register V13_B5
    V13_B5 = 0x1d8,
    /// System register V13_B6
    V13_B6 = 0x1d9,
    /// System register V13_B7
    V13_B7 = 0x1da,
    /// System register V13_B8
    V13_B8 = 0x1db,
    /// System register V13_B9
    V13_B9 = 0x1dc,
    /// System register V13_B10
    V13_B10 = 0x1dd,
    /// System register V13_B11
    V13_B11 = 0x1de,
    /// System register V13_B12
    V13_B12 = 0x1df,
    /// System register V13_B13
    V13_B13 = 0x1e0,
    /// System register V13_B14
    V13_B14 = 0x1e1,
    /// System register V13_B15
    V13_B15 = 0x1e2,
    /// System register V14_B0
    V14_B0 = 0x1e3,
    /// System register V14_B1
    V14_B1 = 0x1e4,
    /// System register V14_B2
    V14_B2 = 0x1e5,
    /// System register V14_B3
    V14_B3 = 0x1e6,
    /// System register V14_B4
    V14_B4 = 0x1e7,
    /// System register V14_B5
    V14_B5 = 0x1e8,
    /// System register V14_B6
    V14_B6 = 0x1e9,
    /// System register V14_B7
    V14_B7 = 0x1ea,
    /// System register V14_B8
    V14_B8 = 0x1eb,
    /// System register V14_B9
    V14_B9 = 0x1ec,
    /// System register V14_B10
    V14_B10 = 0x1ed,
    /// System register V14_B11
    V14_B11 = 0x1ee,
    /// System register V14_B12
    V14_B12 = 0x1ef,
    /// System register V14_B13
    V14_B13 = 0x1f0,
    /// System register V14_B14
    V14_B14 = 0x1f1,
    /// System register V14_B15
    V14_B15 = 0x1f2,
    /// System register V15_B0
    V15_B0 = 0x1f3,
    /// System register V15_B1
    V15_B1 = 0x1f4,
    /// System register V15_B2
    V15_B2 = 0x1f5,
    /// System register V15_B3
    V15_B3 = 0x1f6,
    /// System register V15_B4
    V15_B4 = 0x1f7,
    /// System register V15_B5
    V15_B5 = 0x1f8,
    /// System register V15_B6
    V15_B6 = 0x1f9,
    /// System register V15_B7
    V15_B7 = 0x1fa,
    /// System register V15_B8
    V15_B8 = 0x1fb,
    /// System register V15_B9
    V15_B9 = 0x1fc,
    /// System register V15_B10
    V15_B10 = 0x1fd,
    /// System register V15_B11
    V15_B11 = 0x1fe,
    /// System register V15_B12
    V15_B12 = 0x1ff,
    /// System register V15_B13
    V15_B13 = 0x200,
    /// System register V15_B14
    V15_B14 = 0x201,
    /// System register V15_B15
    V15_B15 = 0x202,
    /// System register V16_B0
    V16_B0 = 0x203,
    /// System register V16_B1
    V16_B1 = 0x204,
    /// System register V16_B2
    V16_B2 = 0x205,
    /// System register V16_B3
    V16_B3 = 0x206,
    /// System register V16_B4
    V16_B4 = 0x207,
    /// System register V16_B5
    V16_B5 = 0x208,
    /// System register V16_B6
    V16_B6 = 0x209,
    /// System register V16_B7
    V16_B7 = 0x20a,
    /// System register V16_B8
    V16_B8 = 0x20b,
    /// System register V16_B9
    V16_B9 = 0x20c,
    /// System register V16_B10
    V16_B10 = 0x20d,
    /// System register V16_B11
    V16_B11 = 0x20e,
    /// System register V16_B12
    V16_B12 = 0x20f,
    /// System register V16_B13
    V16_B13 = 0x210,
    /// System register V16_B14
    V16_B14 = 0x211,
    /// System register V16_B15
    V16_B15 = 0x212,
    /// System register V17_B0
    V17_B0 = 0x213,
    /// System register V17_B1
    V17_B1 = 0x214,
    /// System register V17_B2
    V17_B2 = 0x215,
    /// System register V17_B3
    V17_B3 = 0x216,
    /// System register V17_B4
    V17_B4 = 0x217,
    /// System register V17_B5
    V17_B5 = 0x218,
    /// System register V17_B6
    V17_B6 = 0x219,
    /// System register V17_B7
    V17_B7 = 0x21a,
    /// System register V17_B8
    V17_B8 = 0x21b,
    /// System register V17_B9
    V17_B9 = 0x21c,
    /// System register V17_B10
    V17_B10 = 0x21d,
    /// System register V17_B11
    V17_B11 = 0x21e,
    /// System register V17_B12
    V17_B12 = 0x21f,
    /// System register V17_B13
    V17_B13 = 0x220,
    /// System register V17_B14
    V17_B14 = 0x221,
    /// System register V17_B15
    V17_B15 = 0x222,
    /// System register V18_B0
    V18_B0 = 0x223,
    /// System register V18_B1
    V18_B1 = 0x224,
    /// System register V18_B2
    V18_B2 = 0x225,
    /// System register V18_B3
    V18_B3 = 0x226,
    /// System register V18_B4
    V18_B4 = 0x227,
    /// System register V18_B5
    V18_B5 = 0x228,
    /// System register V18_B6
    V18_B6 = 0x229,
    /// System register V18_B7
    V18_B7 = 0x22a,
    /// System register V18_B8
    V18_B8 = 0x22b,
    /// System register V18_B9
    V18_B9 = 0x22c,
    /// System register V18_B10
    V18_B10 = 0x22d,
    /// System register V18_B11
    V18_B11 = 0x22e,
    /// System register V18_B12
    V18_B12 = 0x22f,
    /// System register V18_B13
    V18_B13 = 0x230,
    /// System register V18_B14
    V18_B14 = 0x231,
    /// System register V18_B15
    V18_B15 = 0x232,
    /// System register V19_B0
    V19_B0 = 0x233,
    /// System register V19_B1
    V19_B1 = 0x234,
    /// System register V19_B2
    V19_B2 = 0x235,
    /// System register V19_B3
    V19_B3 = 0x236,
    /// System register V19_B4
    V19_B4 = 0x237,
    /// System register V19_B5
    V19_B5 = 0x238,
    /// System register V19_B6
    V19_B6 = 0x239,
    /// System register V19_B7
    V19_B7 = 0x23a,
    /// System register V19_B8
    V19_B8 = 0x23b,
    /// System register V19_B9
    V19_B9 = 0x23c,
    /// System register V19_B10
    V19_B10 = 0x23d,
    /// System register V19_B11
    V19_B11 = 0x23e,
    /// System register V19_B12
    V19_B12 = 0x23f,
    /// System register V19_B13
    V19_B13 = 0x240,
    /// System register V19_B14
    V19_B14 = 0x241,
    /// System register V19_B15
    V19_B15 = 0x242,
    /// System register V20_B0
    V20_B0 = 0x243,
    /// System register V20_B1
    V20_B1 = 0x244,
    /// System register V20_B2
    V20_B2 = 0x245,
    /// System register V20_B3
    V20_B3 = 0x246,
    /// System register V20_B4
    V20_B4 = 0x247,
    /// System register V20_B5
    V20_B5 = 0x248,
    /// System register V20_B6
    V20_B6 = 0x249,
    /// System register V20_B7
    V20_B7 = 0x24a,
    /// System register V20_B8
    V20_B8 = 0x24b,
    /// System register V20_B9
    V20_B9 = 0x24c,
    /// System register V20_B10
    V20_B10 = 0x24d,
    /// System register V20_B11
    V20_B11 = 0x24e,
    /// System register V20_B12
    V20_B12 = 0x24f,
    /// System register V20_B13
    V20_B13 = 0x250,
    /// System register V20_B14
    V20_B14 = 0x251,
    /// System register V20_B15
    V20_B15 = 0x252,
    /// System register V21_B0
    V21_B0 = 0x253,
    /// System register V21_B1
    V21_B1 = 0x254,
    /// System register V21_B2
    V21_B2 = 0x255,
    /// System register V21_B3
    V21_B3 = 0x256,
    /// System register V21_B4
    V21_B4 = 0x257,
    /// System register V21_B5
    V21_B5 = 0x258,
    /// System register V21_B6
    V21_B6 = 0x259,
    /// System register V21_B7
    V21_B7 = 0x25a,
    /// System register V21_B8
    V21_B8 = 0x25b,
    /// System register V21_B9
    V21_B9 = 0x25c,
    /// System register V21_B10
    V21_B10 = 0x25d,
    /// System register V21_B11
    V21_B11 = 0x25e,
    /// System register V21_B12
    V21_B12 = 0x25f,
    /// System register V21_B13
    V21_B13 = 0x260,
    /// System register V21_B14
    V21_B14 = 0x261,
    /// System register V21_B15
    V21_B15 = 0x262,
    /// System register V22_B0
    V22_B0 = 0x263,
    /// System register V22_B1
    V22_B1 = 0x264,
    /// System register V22_B2
    V22_B2 = 0x265,
    /// System register V22_B3
    V22_B3 = 0x266,
    /// System register V22_B4
    V22_B4 = 0x267,
    /// System register V22_B5
    V22_B5 = 0x268,
    /// System register V22_B6
    V22_B6 = 0x269,
    /// System register V22_B7
    V22_B7 = 0x26a,
    /// System register V22_B8
    V22_B8 = 0x26b,
    /// System register V22_B9
    V22_B9 = 0x26c,
    /// System register V22_B10
    V22_B10 = 0x26d,
    /// System register V22_B11
    V22_B11 = 0x26e,
    /// System register V22_B12
    V22_B12 = 0x26f,
    /// System register V22_B13
    V22_B13 = 0x270,
    /// System register V22_B14
    V22_B14 = 0x271,
    /// System register V22_B15
    V22_B15 = 0x272,
    /// System register V23_B0
    V23_B0 = 0x273,
    /// System register V23_B1
    V23_B1 = 0x274,
    /// System register V23_B2
    V23_B2 = 0x275,
    /// System register V23_B3
    V23_B3 = 0x276,
    /// System register V23_B4
    V23_B4 = 0x277,
    /// System register V23_B5
    V23_B5 = 0x278,
    /// System register V23_B6
    V23_B6 = 0x279,
    /// System register V23_B7
    V23_B7 = 0x27a,
    /// System register V23_B8
    V23_B8 = 0x27b,
    /// System register V23_B9
    V23_B9 = 0x27c,
    /// System register V23_B10
    V23_B10 = 0x27d,
    /// System register V23_B11
    V23_B11 = 0x27e,
    /// System register V23_B12
    V23_B12 = 0x27f,
    /// System register V23_B13
    V23_B13 = 0x280,
    /// System register V23_B14
    V23_B14 = 0x281,
    /// System register V23_B15
    V23_B15 = 0x282,
    /// System register V24_B0
    V24_B0 = 0x283,
    /// System register V24_B1
    V24_B1 = 0x284,
    /// System register V24_B2
    V24_B2 = 0x285,
    /// System register V24_B3
    V24_B3 = 0x286,
    /// System register V24_B4
    V24_B4 = 0x287,
    /// System register V24_B5
    V24_B5 = 0x288,
    /// System register V24_B6
    V24_B6 = 0x289,
    /// System register V24_B7
    V24_B7 = 0x28a,
    /// System register V24_B8
    V24_B8 = 0x28b,
    /// System register V24_B9
    V24_B9 = 0x28c,
    /// System register V24_B10
    V24_B10 = 0x28d,
    /// System register V24_B11
    V24_B11 = 0x28e,
    /// System register V24_B12
    V24_B12 = 0x28f,
    /// System register V24_B13
    V24_B13 = 0x290,
    /// System register V24_B14
    V24_B14 = 0x291,
    /// System register V24_B15
    V24_B15 = 0x292,
    /// System register V25_B0
    V25_B0 = 0x293,
    /// System register V25_B1
    V25_B1 = 0x294,
    /// System register V25_B2
    V25_B2 = 0x295,
    /// System register V25_B3
    V25_B3 = 0x296,
    /// System register V25_B4
    V25_B4 = 0x297,
    /// System register V25_B5
    V25_B5 = 0x298,
    /// System register V25_B6
    V25_B6 = 0x299,
    /// System register V25_B7
    V25_B7 = 0x29a,
    /// System register V25_B8
    V25_B8 = 0x29b,
    /// System register V25_B9
    V25_B9 = 0x29c,
    /// System register V25_B10
    V25_B10 = 0x29d,
    /// System register V25_B11
    V25_B11 = 0x29e,
    /// System register V25_B12
    V25_B12 = 0x29f,
    /// System register V25_B13
    V25_B13 = 0x2a0,
    /// System register V25_B14
    V25_B14 = 0x2a1,
    /// System register V25_B15
    V25_B15 = 0x2a2,
    /// System register V26_B0
    V26_B0 = 0x2a3,
    /// System register V26_B1
    V26_B1 = 0x2a4,
    /// System register V26_B2
    V26_B2 = 0x2a5,
    /// System register V26_B3
    V26_B3 = 0x2a6,
    /// System register V26_B4
    V26_B4 = 0x2a7,
    /// System register V26_B5
    V26_B5 = 0x2a8,
    /// System register V26_B6
    V26_B6 = 0x2a9,
    /// System register V26_B7
    V26_B7 = 0x2aa,
    /// System register V26_B8
    V26_B8 = 0x2ab,
    /// System register V26_B9
    V26_B9 = 0x2ac,
    /// System register V26_B10
    V26_B10 = 0x2ad,
    /// System register V26_B11
    V26_B11 = 0x2ae,
    /// System register V26_B12
    V26_B12 = 0x2af,
    /// System register V26_B13
    V26_B13 = 0x2b0,
    /// System register V26_B14
    V26_B14 = 0x2b1,
    /// System register V26_B15
    V26_B15 = 0x2b2,
    /// System register V27_B0
    V27_B0 = 0x2b3,
    /// System register V27_B1
    V27_B1 = 0x2b4,
    /// System register V27_B2
    V27_B2 = 0x2b5,
    /// System register V27_B3
    V27_B3 = 0x2b6,
    /// System register V27_B4
    V27_B4 = 0x2b7,
    /// System register V27_B5
    V27_B5 = 0x2b8,
    /// System register V27_B6
    V27_B6 = 0x2b9,
    /// System register V27_B7
    V27_B7 = 0x2ba,
    /// System register V27_B8
    V27_B8 = 0x2bb,
    /// System register V27_B9
    V27_B9 = 0x2bc,
    /// System register V27_B10
    V27_B10 = 0x2bd,
    /// System register V27_B11
    V27_B11 = 0x2be,
    /// System register V27_B12
    V27_B12 = 0x2bf,
    /// System register V27_B13
    V27_B13 = 0x2c0,
    /// System register V27_B14
    V27_B14 = 0x2c1,
    /// System register V27_B15
    V27_B15 = 0x2c2,
    /// System register V28_B0
    V28_B0 = 0x2c3,
    /// System register V28_B1
    V28_B1 = 0x2c4,
    /// System register V28_B2
    V28_B2 = 0x2c5,
    /// System register V28_B3
    V28_B3 = 0x2c6,
    /// System register V28_B4
    V28_B4 = 0x2c7,
    /// System register V28_B5
    V28_B5 = 0x2c8,
    /// System register V28_B6
    V28_B6 = 0x2c9,
    /// System register V28_B7
    V28_B7 = 0x2ca,
    /// System register V28_B8
    V28_B8 = 0x2cb,
    /// System register V28_B9
    V28_B9 = 0x2cc,
    /// System register V28_B10
    V28_B10 = 0x2cd,
    /// System register V28_B11
    V28_B11 = 0x2ce,
    /// System register V28_B12
    V28_B12 = 0x2cf,
    /// System register V28_B13
    V28_B13 = 0x2d0,
    /// System register V28_B14
    V28_B14 = 0x2d1,
    /// System register V28_B15
    V28_B15 = 0x2d2,
    /// System register V29_B0
    V29_B0 = 0x2d3,
    /// System register V29_B1
    V29_B1 = 0x2d4,
    /// System register V29_B2
    V29_B2 = 0x2d5,
    /// System register V29_B3
    V29_B3 = 0x2d6,
    /// System register V29_B4
    V29_B4 = 0x2d7,
    /// System register V29_B5
    V29_B5 = 0x2d8,
    /// System register V29_B6
    V29_B6 = 0x2d9,
    /// System register V29_B7
    V29_B7 = 0x2da,
    /// System register V29_B8
    V29_B8 = 0x2db,
    /// System register V29_B9
    V29_B9 = 0x2dc,
    /// System register V29_B10
    V29_B10 = 0x2dd,
    /// System register V29_B11
    V29_B11 = 0x2de,
    /// System register V29_B12
    V29_B12 = 0x2df,
    /// System register V29_B13
    V29_B13 = 0x2e0,
    /// System register V29_B14
    V29_B14 = 0x2e1,
    /// System register V29_B15
    V29_B15 = 0x2e2,
    /// System register V30_B0
    V30_B0 = 0x2e3,
    /// System register V30_B1
    V30_B1 = 0x2e4,
    /// System register V30_B2
    V30_B2 = 0x2e5,
    /// System register V30_B3
    V30_B3 = 0x2e6,
    /// System register V30_B4
    V30_B4 = 0x2e7,
    /// System register V30_B5
    V30_B5 = 0x2e8,
    /// System register V30_B6
    V30_B6 = 0x2e9,
    /// System register V30_B7
    V30_B7 = 0x2ea,
    /// System register V30_B8
    V30_B8 = 0x2eb,
    /// System register V30_B9
    V30_B9 = 0x2ec,
    /// System register V30_B10
    V30_B10 = 0x2ed,
    /// System register V30_B11
    V30_B11 = 0x2ee,
    /// System register V30_B12
    V30_B12 = 0x2ef,
    /// System register V30_B13
    V30_B13 = 0x2f0,
    /// System register V30_B14
    V30_B14 = 0x2f1,
    /// System register V30_B15
    V30_B15 = 0x2f2,
    /// System register V31_B0
    V31_B0 = 0x2f3,
    /// System register V31_B1
    V31_B1 = 0x2f4,
    /// System register V31_B2
    V31_B2 = 0x2f5,
    /// System register V31_B3
    V31_B3 = 0x2f6,
    /// System register V31_B4
    V31_B4 = 0x2f7,
    /// System register V31_B5
    V31_B5 = 0x2f8,
    /// System register V31_B6
    V31_B6 = 0x2f9,
    /// System register V31_B7
    V31_B7 = 0x2fa,
    /// System register V31_B8
    V31_B8 = 0x2fb,
    /// System register V31_B9
    V31_B9 = 0x2fc,
    /// System register V31_B10
    V31_B10 = 0x2fd,
    /// System register V31_B11
    V31_B11 = 0x2fe,
    /// System register V31_B12
    V31_B12 = 0x2ff,
    /// System register V31_B13
    V31_B13 = 0x300,
    /// System register V31_B14
    V31_B14 = 0x301,
    /// System register V31_B15
    V31_B15 = 0x302,
    /// System register V0_H0
    V0_H0 = 0x303,
    /// System register V0_H1
    V0_H1 = 0x304,
    /// System register V0_H2
    V0_H2 = 0x305,
    /// System register V0_H3
    V0_H3 = 0x306,
    /// System register V0_H4
    V0_H4 = 0x307,
    /// System register V0_H5
    V0_H5 = 0x308,
    /// System register V0_H6
    V0_H6 = 0x309,
    /// System register V0_H7
    V0_H7 = 0x30a,
    /// System register V1_H0
    V1_H0 = 0x30b,
    /// System register V1_H1
    V1_H1 = 0x30c,
    /// System register V1_H2
    V1_H2 = 0x30d,
    /// System register V1_H3
    V1_H3 = 0x30e,
    /// System register V1_H4
    V1_H4 = 0x30f,
    /// System register V1_H5
    V1_H5 = 0x310,
    /// System register V1_H6
    V1_H6 = 0x311,
    /// System register V1_H7
    V1_H7 = 0x312,
    /// System register V2_H0
    V2_H0 = 0x313,
    /// System register V2_H1
    V2_H1 = 0x314,
    /// System register V2_H2
    V2_H2 = 0x315,
    /// System register V2_H3
    V2_H3 = 0x316,
    /// System register V2_H4
    V2_H4 = 0x317,
    /// System register V2_H5
    V2_H5 = 0x318,
    /// System register V2_H6
    V2_H6 = 0x319,
    /// System register V2_H7
    V2_H7 = 0x31a,
    /// System register V3_H0
    V3_H0 = 0x31b,
    /// System register V3_H1
    V3_H1 = 0x31c,
    /// System register V3_H2
    V3_H2 = 0x31d,
    /// System register V3_H3
    V3_H3 = 0x31e,
    /// System register V3_H4
    V3_H4 = 0x31f,
    /// System register V3_H5
    V3_H5 = 0x320,
    /// System register V3_H6
    V3_H6 = 0x321,
    /// System register V3_H7
    V3_H7 = 0x322,
    /// System register V4_H0
    V4_H0 = 0x323,
    /// System register V4_H1
    V4_H1 = 0x324,
    /// System register V4_H2
    V4_H2 = 0x325,
    /// System register V4_H3
    V4_H3 = 0x326,
    /// System register V4_H4
    V4_H4 = 0x327,
    /// System register V4_H5
    V4_H5 = 0x328,
    /// System register V4_H6
    V4_H6 = 0x329,
    /// System register V4_H7
    V4_H7 = 0x32a,
    /// System register V5_H0
    V5_H0 = 0x32b,
    /// System register V5_H1
    V5_H1 = 0x32c,
    /// System register V5_H2
    V5_H2 = 0x32d,
    /// System register V5_H3
    V5_H3 = 0x32e,
    /// System register V5_H4
    V5_H4 = 0x32f,
    /// System register V5_H5
    V5_H5 = 0x330,
    /// System register V5_H6
    V5_H6 = 0x331,
    /// System register V5_H7
    V5_H7 = 0x332,
    /// System register V6_H0
    V6_H0 = 0x333,
    /// System register V6_H1
    V6_H1 = 0x334,
    /// System register V6_H2
    V6_H2 = 0x335,
    /// System register V6_H3
    V6_H3 = 0x336,
    /// System register V6_H4
    V6_H4 = 0x337,
    /// System register V6_H5
    V6_H5 = 0x338,
    /// System register V6_H6
    V6_H6 = 0x339,
    /// System register V6_H7
    V6_H7 = 0x33a,
    /// System register V7_H0
    V7_H0 = 0x33b,
    /// System register V7_H1
    V7_H1 = 0x33c,
    /// System register V7_H2
    V7_H2 = 0x33d,
    /// System register V7_H3
    V7_H3 = 0x33e,
    /// System register V7_H4
    V7_H4 = 0x33f,
    /// System register V7_H5
    V7_H5 = 0x340,
    /// System register V7_H6
    V7_H6 = 0x341,
    /// System register V7_H7
    V7_H7 = 0x342,
    /// System register V8_H0
    V8_H0 = 0x343,
    /// System register V8_H1
    V8_H1 = 0x344,
    /// System register V8_H2
    V8_H2 = 0x345,
    /// System register V8_H3
    V8_H3 = 0x346,
    /// System register V8_H4
    V8_H4 = 0x347,
    /// System register V8_H5
    V8_H5 = 0x348,
    /// System register V8_H6
    V8_H6 = 0x349,
    /// System register V8_H7
    V8_H7 = 0x34a,
    /// System register V9_H0
    V9_H0 = 0x34b,
    /// System register V9_H1
    V9_H1 = 0x34c,
    /// System register V9_H2
    V9_H2 = 0x34d,
    /// System register V9_H3
    V9_H3 = 0x34e,
    /// System register V9_H4
    V9_H4 = 0x34f,
    /// System register V9_H5
    V9_H5 = 0x350,
    /// System register V9_H6
    V9_H6 = 0x351,
    /// System register V9_H7
    V9_H7 = 0x352,
    /// System register V10_H0
    V10_H0 = 0x353,
    /// System register V10_H1
    V10_H1 = 0x354,
    /// System register V10_H2
    V10_H2 = 0x355,
    /// System register V10_H3
    V10_H3 = 0x356,
    /// System register V10_H4
    V10_H4 = 0x357,
    /// System register V10_H5
    V10_H5 = 0x358,
    /// System register V10_H6
    V10_H6 = 0x359,
    /// System register V10_H7
    V10_H7 = 0x35a,
    /// System register V11_H0
    V11_H0 = 0x35b,
    /// System register V11_H1
    V11_H1 = 0x35c,
    /// System register V11_H2
    V11_H2 = 0x35d,
    /// System register V11_H3
    V11_H3 = 0x35e,
    /// System register V11_H4
    V11_H4 = 0x35f,
    /// System register V11_H5
    V11_H5 = 0x360,
    /// System register V11_H6
    V11_H6 = 0x361,
    /// System register V11_H7
    V11_H7 = 0x362,
    /// System register V12_H0
    V12_H0 = 0x363,
    /// System register V12_H1
    V12_H1 = 0x364,
    /// System register V12_H2
    V12_H2 = 0x365,
    /// System register V12_H3
    V12_H3 = 0x366,
    /// System register V12_H4
    V12_H4 = 0x367,
    /// System register V12_H5
    V12_H5 = 0x368,
    /// System register V12_H6
    V12_H6 = 0x369,
    /// System register V12_H7
    V12_H7 = 0x36a,
    /// System register V13_H0
    V13_H0 = 0x36b,
    /// System register V13_H1
    V13_H1 = 0x36c,
    /// System register V13_H2
    V13_H2 = 0x36d,
    /// System register V13_H3
    V13_H3 = 0x36e,
    /// System register V13_H4
    V13_H4 = 0x36f,
    /// System register V13_H5
    V13_H5 = 0x370,
    /// System register V13_H6
    V13_H6 = 0x371,
    /// System register V13_H7
    V13_H7 = 0x372,
    /// System register V14_H0
    V14_H0 = 0x373,
    /// System register V14_H1
    V14_H1 = 0x374,
    /// System register V14_H2
    V14_H2 = 0x375,
    /// System register V14_H3
    V14_H3 = 0x376,
    /// System register V14_H4
    V14_H4 = 0x377,
    /// System register V14_H5
    V14_H5 = 0x378,
    /// System register V14_H6
    V14_H6 = 0x379,
    /// System register V14_H7
    V14_H7 = 0x37a,
    /// System register V15_H0
    V15_H0 = 0x37b,
    /// System register V15_H1
    V15_H1 = 0x37c,
    /// System register V15_H2
    V15_H2 = 0x37d,
    /// System register V15_H3
    V15_H3 = 0x37e,
    /// System register V15_H4
    V15_H4 = 0x37f,
    /// System register V15_H5
    V15_H5 = 0x380,
    /// System register V15_H6
    V15_H6 = 0x381,
    /// System register V15_H7
    V15_H7 = 0x382,
    /// System register V16_H0
    V16_H0 = 0x383,
    /// System register V16_H1
    V16_H1 = 0x384,
    /// System register V16_H2
    V16_H2 = 0x385,
    /// System register V16_H3
    V16_H3 = 0x386,
    /// System register V16_H4
    V16_H4 = 0x387,
    /// System register V16_H5
    V16_H5 = 0x388,
    /// System register V16_H6
    V16_H6 = 0x389,
    /// System register V16_H7
    V16_H7 = 0x38a,
    /// System register V17_H0
    V17_H0 = 0x38b,
    /// System register V17_H1
    V17_H1 = 0x38c,
    /// System register V17_H2
    V17_H2 = 0x38d,
    /// System register V17_H3
    V17_H3 = 0x38e,
    /// System register V17_H4
    V17_H4 = 0x38f,
    /// System register V17_H5
    V17_H5 = 0x390,
    /// System register V17_H6
    V17_H6 = 0x391,
    /// System register V17_H7
    V17_H7 = 0x392,
    /// System register V18_H0
    V18_H0 = 0x393,
    /// System register V18_H1
    V18_H1 = 0x394,
    /// System register V18_H2
    V18_H2 = 0x395,
    /// System register V18_H3
    V18_H3 = 0x396,
    /// System register V18_H4
    V18_H4 = 0x397,
    /// System register V18_H5
    V18_H5 = 0x398,
    /// System register V18_H6
    V18_H6 = 0x399,
    /// System register V18_H7
    V18_H7 = 0x39a,
    /// System register V19_H0
    V19_H0 = 0x39b,
    /// System register V19_H1
    V19_H1 = 0x39c,
    /// System register V19_H2
    V19_H2 = 0x39d,
    /// System register V19_H3
    V19_H3 = 0x39e,
    /// System register V19_H4
    V19_H4 = 0x39f,
    /// System register V19_H5
    V19_H5 = 0x3a0,
    /// System register V19_H6
    V19_H6 = 0x3a1,
    /// System register V19_H7
    V19_H7 = 0x3a2,
    /// System register V20_H0
    V20_H0 = 0x3a3,
    /// System register V20_H1
    V20_H1 = 0x3a4,
    /// System register V20_H2
    V20_H2 = 0x3a5,
    /// System register V20_H3
    V20_H3 = 0x3a6,
    /// System register V20_H4
    V20_H4 = 0x3a7,
    /// System register V20_H5
    V20_H5 = 0x3a8,
    /// System register V20_H6
    V20_H6 = 0x3a9,
    /// System register V20_H7
    V20_H7 = 0x3aa,
    /// System register V21_H0
    V21_H0 = 0x3ab,
    /// System register V21_H1
    V21_H1 = 0x3ac,
    /// System register V21_H2
    V21_H2 = 0x3ad,
    /// System register V21_H3
    V21_H3 = 0x3ae,
    /// System register V21_H4
    V21_H4 = 0x3af,
    /// System register V21_H5
    V21_H5 = 0x3b0,
    /// System register V21_H6
    V21_H6 = 0x3b1,
    /// System register V21_H7
    V21_H7 = 0x3b2,
    /// System register V22_H0
    V22_H0 = 0x3b3,
    /// System register V22_H1
    V22_H1 = 0x3b4,
    /// System register V22_H2
    V22_H2 = 0x3b5,
    /// System register V22_H3
    V22_H3 = 0x3b6,
    /// System register V22_H4
    V22_H4 = 0x3b7,
    /// System register V22_H5
    V22_H5 = 0x3b8,
    /// System register V22_H6
    V22_H6 = 0x3b9,
    /// System register V22_H7
    V22_H7 = 0x3ba,
    /// System register V23_H0
    V23_H0 = 0x3bb,
    /// System register V23_H1
    V23_H1 = 0x3bc,
    /// System register V23_H2
    V23_H2 = 0x3bd,
    /// System register V23_H3
    V23_H3 = 0x3be,
    /// System register V23_H4
    V23_H4 = 0x3bf,
    /// System register V23_H5
    V23_H5 = 0x3c0,
    /// System register V23_H6
    V23_H6 = 0x3c1,
    /// System register V23_H7
    V23_H7 = 0x3c2,
    /// System register V24_H0
    V24_H0 = 0x3c3,
    /// System register V24_H1
    V24_H1 = 0x3c4,
    /// System register V24_H2
    V24_H2 = 0x3c5,
    /// System register V24_H3
    V24_H3 = 0x3c6,
    /// System register V24_H4
    V24_H4 = 0x3c7,
    /// System register V24_H5
    V24_H5 = 0x3c8,
    /// System register V24_H6
    V24_H6 = 0x3c9,
    /// System register V24_H7
    V24_H7 = 0x3ca,
    /// System register V25_H0
    V25_H0 = 0x3cb,
    /// System register V25_H1
    V25_H1 = 0x3cc,
    /// System register V25_H2
    V25_H2 = 0x3cd,
    /// System register V25_H3
    V25_H3 = 0x3ce,
    /// System register V25_H4
    V25_H4 = 0x3cf,
    /// System register V25_H5
    V25_H5 = 0x3d0,
    /// System register V25_H6
    V25_H6 = 0x3d1,
    /// System register V25_H7
    V25_H7 = 0x3d2,
    /// System register V26_H0
    V26_H0 = 0x3d3,
    /// System register V26_H1
    V26_H1 = 0x3d4,
    /// System register V26_H2
    V26_H2 = 0x3d5,
    /// System register V26_H3
    V26_H3 = 0x3d6,
    /// System register V26_H4
    V26_H4 = 0x3d7,
    /// System register V26_H5
    V26_H5 = 0x3d8,
    /// System register V26_H6
    V26_H6 = 0x3d9,
    /// System register V26_H7
    V26_H7 = 0x3da,
    /// System register V27_H0
    V27_H0 = 0x3db,
    /// System register V27_H1
    V27_H1 = 0x3dc,
    /// System register V27_H2
    V27_H2 = 0x3dd,
    /// System register V27_H3
    V27_H3 = 0x3de,
    /// System register V27_H4
    V27_H4 = 0x3df,
    /// System register V27_H5
    V27_H5 = 0x3e0,
    /// System register V27_H6
    V27_H6 = 0x3e1,
    /// System register V27_H7
    V27_H7 = 0x3e2,
    /// System register V28_H0
    V28_H0 = 0x3e3,
    /// System register V28_H1
    V28_H1 = 0x3e4,
    /// System register V28_H2
    V28_H2 = 0x3e5,
    /// System register V28_H3
    V28_H3 = 0x3e6,
    /// System register V28_H4
    V28_H4 = 0x3e7,
    /// System register V28_H5
    V28_H5 = 0x3e8,
    /// System register V28_H6
    V28_H6 = 0x3e9,
    /// System register V28_H7
    V28_H7 = 0x3ea,
    /// System register V29_H0
    V29_H0 = 0x3eb,
    /// System register V29_H1
    V29_H1 = 0x3ec,
    /// System register V29_H2
    V29_H2 = 0x3ed,
    /// System register V29_H3
    V29_H3 = 0x3ee,
    /// System register V29_H4
    V29_H4 = 0x3ef,
    /// System register V29_H5
    V29_H5 = 0x3f0,
    /// System register V29_H6
    V29_H6 = 0x3f1,
    /// System register V29_H7
    V29_H7 = 0x3f2,
    /// System register V30_H0
    V30_H0 = 0x3f3,
    /// System register V30_H1
    V30_H1 = 0x3f4,
    /// System register V30_H2
    V30_H2 = 0x3f5,
    /// System register V30_H3
    V30_H3 = 0x3f6,
    /// System register V30_H4
    V30_H4 = 0x3f7,
    /// System register V30_H5
    V30_H5 = 0x3f8,
    /// System register V30_H6
    V30_H6 = 0x3f9,
    /// System register V30_H7
    V30_H7 = 0x3fa,
    /// System register V31_H0
    V31_H0 = 0x3fb,
    /// System register V31_H1
    V31_H1 = 0x3fc,
    /// System register V31_H2
    V31_H2 = 0x3fd,
    /// System register V31_H3
    V31_H3 = 0x3fe,
    /// System register V31_H4
    V31_H4 = 0x3ff,
    /// System register V31_H5
    V31_H5 = 0x400,
    /// System register V31_H6
    V31_H6 = 0x401,
    /// System register V31_H7
    V31_H7 = 0x402,
    /// System register V0_S0
    V0_S0 = 0x403,
    /// System register V0_S1
    V0_S1 = 0x404,
    /// System register V0_S2
    V0_S2 = 0x405,
    /// System register V0_S3
    V0_S3 = 0x406,
    /// System register V1_S0
    V1_S0 = 0x407,
    /// System register V1_S1
    V1_S1 = 0x408,
    /// System register V1_S2
    V1_S2 = 0x409,
    /// System register V1_S3
    V1_S3 = 0x40a,
    /// System register V2_S0
    V2_S0 = 0x40b,
    /// System register V2_S1
    V2_S1 = 0x40c,
    /// System register V2_S2
    V2_S2 = 0x40d,
    /// System register V2_S3
    V2_S3 = 0x40e,
    /// System register V3_S0
    V3_S0 = 0x40f,
    /// System register V3_S1
    V3_S1 = 0x410,
    /// System register V3_S2
    V3_S2 = 0x411,
    /// System register V3_S3
    V3_S3 = 0x412,
    /// System register V4_S0
    V4_S0 = 0x413,
    /// System register V4_S1
    V4_S1 = 0x414,
    /// System register V4_S2
    V4_S2 = 0x415,
    /// System register V4_S3
    V4_S3 = 0x416,
    /// System register V5_S0
    V5_S0 = 0x417,
    /// System register V5_S1
    V5_S1 = 0x418,
    /// System register V5_S2
    V5_S2 = 0x419,
    /// System register V5_S3
    V5_S3 = 0x41a,
    /// System register V6_S0
    V6_S0 = 0x41b,
    /// System register V6_S1
    V6_S1 = 0x41c,
    /// System register V6_S2
    V6_S2 = 0x41d,
    /// System register V6_S3
    V6_S3 = 0x41e,
    /// System register V7_S0
    V7_S0 = 0x41f,
    /// System register V7_S1
    V7_S1 = 0x420,
    /// System register V7_S2
    V7_S2 = 0x421,
    /// System register V7_S3
    V7_S3 = 0x422,
    /// System register V8_S0
    V8_S0 = 0x423,
    /// System register V8_S1
    V8_S1 = 0x424,
    /// System register V8_S2
    V8_S2 = 0x425,
    /// System register V8_S3
    V8_S3 = 0x426,
    /// System register V9_S0
    V9_S0 = 0x427,
    /// System register V9_S1
    V9_S1 = 0x428,
    /// System register V9_S2
    V9_S2 = 0x429,
    /// System register V9_S3
    V9_S3 = 0x42a,
    /// System register V10_S0
    V10_S0 = 0x42b,
    /// System register V10_S1
    V10_S1 = 0x42c,
    /// System register V10_S2
    V10_S2 = 0x42d,
    /// System register V10_S3
    V10_S3 = 0x42e,
    /// System register V11_S0
    V11_S0 = 0x42f,
    /// System register V11_S1
    V11_S1 = 0x430,
    /// System register V11_S2
    V11_S2 = 0x431,
    /// System register V11_S3
    V11_S3 = 0x432,
    /// System register V12_S0
    V12_S0 = 0x433,
    /// System register V12_S1
    V12_S1 = 0x434,
    /// System register V12_S2
    V12_S2 = 0x435,
    /// System register V12_S3
    V12_S3 = 0x436,
    /// System register V13_S0
    V13_S0 = 0x437,
    /// System register V13_S1
    V13_S1 = 0x438,
    /// System register V13_S2
    V13_S2 = 0x439,
    /// System register V13_S3
    V13_S3 = 0x43a,
    /// System register V14_S0
    V14_S0 = 0x43b,
    /// System register V14_S1
    V14_S1 = 0x43c,
    /// System register V14_S2
    V14_S2 = 0x43d,
    /// System register V14_S3
    V14_S3 = 0x43e,
    /// System register V15_S0
    V15_S0 = 0x43f,
    /// System register V15_S1
    V15_S1 = 0x440,
    /// System register V15_S2
    V15_S2 = 0x441,
    /// System register V15_S3
    V15_S3 = 0x442,
    /// System register V16_S0
    V16_S0 = 0x443,
    /// System register V16_S1
    V16_S1 = 0x444,
    /// System register V16_S2
    V16_S2 = 0x445,
    /// System register V16_S3
    V16_S3 = 0x446,
    /// System register V17_S0
    V17_S0 = 0x447,
    /// System register V17_S1
    V17_S1 = 0x448,
    /// System register V17_S2
    V17_S2 = 0x449,
    /// System register V17_S3
    V17_S3 = 0x44a,
    /// System register V18_S0
    V18_S0 = 0x44b,
    /// System register V18_S1
    V18_S1 = 0x44c,
    /// System register V18_S2
    V18_S2 = 0x44d,
    /// System register V18_S3
    V18_S3 = 0x44e,
    /// System register V19_S0
    V19_S0 = 0x44f,
    /// System register V19_S1
    V19_S1 = 0x450,
    /// System register V19_S2
    V19_S2 = 0x451,
    /// System register V19_S3
    V19_S3 = 0x452,
    /// System register V20_S0
    V20_S0 = 0x453,
    /// System register V20_S1
    V20_S1 = 0x454,
    /// System register V20_S2
    V20_S2 = 0x455,
    /// System register V20_S3
    V20_S3 = 0x456,
    /// System register V21_S0
    V21_S0 = 0x457,
    /// System register V21_S1
    V21_S1 = 0x458,
    /// System register V21_S2
    V21_S2 = 0x459,
    /// System register V21_S3
    V21_S3 = 0x45a,
    /// System register V22_S0
    V22_S0 = 0x45b,
    /// System register V22_S1
    V22_S1 = 0x45c,
    /// System register V22_S2
    V22_S2 = 0x45d,
    /// System register V22_S3
    V22_S3 = 0x45e,
    /// System register V23_S0
    V23_S0 = 0x45f,
    /// System register V23_S1
    V23_S1 = 0x460,
    /// System register V23_S2
    V23_S2 = 0x461,
    /// System register V23_S3
    V23_S3 = 0x462,
    /// System register V24_S0
    V24_S0 = 0x463,
    /// System register V24_S1
    V24_S1 = 0x464,
    /// System register V24_S2
    V24_S2 = 0x465,
    /// System register V24_S3
    V24_S3 = 0x466,
    /// System register V25_S0
    V25_S0 = 0x467,
    /// System register V25_S1
    V25_S1 = 0x468,
    /// System register V25_S2
    V25_S2 = 0x469,
    /// System register V25_S3
    V25_S3 = 0x46a,
    /// System register V26_S0
    V26_S0 = 0x46b,
    /// System register V26_S1
    V26_S1 = 0x46c,
    /// System register V26_S2
    V26_S2 = 0x46d,
    /// System register V26_S3
    V26_S3 = 0x46e,
    /// System register V27_S0
    V27_S0 = 0x46f,
    /// System register V27_S1
    V27_S1 = 0x470,
    /// System register V27_S2
    V27_S2 = 0x471,
    /// System register V27_S3
    V27_S3 = 0x472,
    /// System register V28_S0
    V28_S0 = 0x473,
    /// System register V28_S1
    V28_S1 = 0x474,
    /// System register V28_S2
    V28_S2 = 0x475,
    /// System register V28_S3
    V28_S3 = 0x476,
    /// System register V29_S0
    V29_S0 = 0x477,
    /// System register V29_S1
    V29_S1 = 0x478,
    /// System register V29_S2
    V29_S2 = 0x479,
    /// System register V29_S3
    V29_S3 = 0x47a,
    /// System register V30_S0
    V30_S0 = 0x47b,
    /// System register V30_S1
    V30_S1 = 0x47c,
    /// System register V30_S2
    V30_S2 = 0x47d,
    /// System register V30_S3
    V30_S3 = 0x47e,
    /// System register V31_S0
    V31_S0 = 0x47f,
    /// System register V31_S1
    V31_S1 = 0x480,
    /// System register V31_S2
    V31_S2 = 0x481,
    /// System register V31_S3
    V31_S3 = 0x482,
    /// System register V0_D0
    V0_D0 = 0x483,
    /// System register V0_D1
    V0_D1 = 0x484,
    /// System register V1_D0
    V1_D0 = 0x485,
    /// System register V1_D1
    V1_D1 = 0x486,
    /// System register V2_D0
    V2_D0 = 0x487,
    /// System register V2_D1
    V2_D1 = 0x488,
    /// System register V3_D0
    V3_D0 = 0x489,
    /// System register V3_D1
    V3_D1 = 0x48a,
    /// System register V4_D0
    V4_D0 = 0x48b,
    /// System register V4_D1
    V4_D1 = 0x48c,
    /// System register V5_D0
    V5_D0 = 0x48d,
    /// System register V5_D1
    V5_D1 = 0x48e,
    /// System register V6_D0
    V6_D0 = 0x48f,
    /// System register V6_D1
    V6_D1 = 0x490,
    /// System register V7_D0
    V7_D0 = 0x491,
    /// System register V7_D1
    V7_D1 = 0x492,
    /// System register V8_D0
    V8_D0 = 0x493,
    /// System register V8_D1
    V8_D1 = 0x494,
    /// System register V9_D0
    V9_D0 = 0x495,
    /// System register V9_D1
    V9_D1 = 0x496,
    /// System register V10_D0
    V10_D0 = 0x497,
    /// System register V10_D1
    V10_D1 = 0x498,
    /// System register V11_D0
    V11_D0 = 0x499,
    /// System register V11_D1
    V11_D1 = 0x49a,
    /// System register V12_D0
    V12_D0 = 0x49b,
    /// System register V12_D1
    V12_D1 = 0x49c,
    /// System register V13_D0
    V13_D0 = 0x49d,
    /// System register V13_D1
    V13_D1 = 0x49e,
    /// System register V14_D0
    V14_D0 = 0x49f,
    /// System register V14_D1
    V14_D1 = 0x4a0,
    /// System register V15_D0
    V15_D0 = 0x4a1,
    /// System register V15_D1
    V15_D1 = 0x4a2,
    /// System register V16_D0
    V16_D0 = 0x4a3,
    /// System register V16_D1
    V16_D1 = 0x4a4,
    /// System register V17_D0
    V17_D0 = 0x4a5,
    /// System register V17_D1
    V17_D1 = 0x4a6,
    /// System register V18_D0
    V18_D0 = 0x4a7,
    /// System register V18_D1
    V18_D1 = 0x4a8,
    /// System register V19_D0
    V19_D0 = 0x4a9,
    /// System register V19_D1
    V19_D1 = 0x4aa,
    /// System register V20_D0
    V20_D0 = 0x4ab,
    /// System register V20_D1
    V20_D1 = 0x4ac,
    /// System register V21_D0
    V21_D0 = 0x4ad,
    /// System register V21_D1
    V21_D1 = 0x4ae,
    /// System register V22_D0
    V22_D0 = 0x4af,
    /// System register V22_D1
    V22_D1 = 0x4b0,
    /// System register V23_D0
    V23_D0 = 0x4b1,
    /// System register V23_D1
    V23_D1 = 0x4b2,
    /// System register V24_D0
    V24_D0 = 0x4b3,
    /// System register V24_D1
    V24_D1 = 0x4b4,
    /// System register V25_D0
    V25_D0 = 0x4b5,
    /// System register V25_D1
    V25_D1 = 0x4b6,
    /// System register V26_D0
    V26_D0 = 0x4b7,
    /// System register V26_D1
    V26_D1 = 0x4b8,
    /// System register V27_D0
    V27_D0 = 0x4b9,
    /// System register V27_D1
    V27_D1 = 0x4ba,
    /// System register V28_D0
    V28_D0 = 0x4bb,
    /// System register V28_D1
    V28_D1 = 0x4bc,
    /// System register V29_D0
    V29_D0 = 0x4bd,
    /// System register V29_D1
    V29_D1 = 0x4be,
    /// System register V30_D0
    V30_D0 = 0x4bf,
    /// System register V30_D1
    V30_D1 = 0x4c0,
    /// System register V31_D0
    V31_D0 = 0x4c1,
    /// System register V31_D1
    V31_D1 = 0x4c2,
    /// System register Z0
    Z0 = 0x4c3,
    /// System register Z1
    Z1 = 0x4c4,
    /// System register Z2
    Z2 = 0x4c5,
    /// System register Z3
    Z3 = 0x4c6,
    /// System register Z4
    Z4 = 0x4c7,
    /// System register Z5
    Z5 = 0x4c8,
    /// System register Z6
    Z6 = 0x4c9,
    /// System register Z7
    Z7 = 0x4ca,
    /// System register Z8
    Z8 = 0x4cb,
    /// System register Z9
    Z9 = 0x4cc,
    /// System register Z10
    Z10 = 0x4cd,
    /// System register Z11
    Z11 = 0x4ce,
    /// System register Z12
    Z12 = 0x4cf,
    /// System register Z13
    Z13 = 0x4d0,
    /// System register Z14
    Z14 = 0x4d1,
    /// System register Z15
    Z15 = 0x4d2,
    /// System register Z16
    Z16 = 0x4d3,
    /// System register Z17
    Z17 = 0x4d4,
    /// System register Z18
    Z18 = 0x4d5,
    /// System register Z19
    Z19 = 0x4d6,
    /// System register Z20
    Z20 = 0x4d7,
    /// System register Z21
    Z21 = 0x4d8,
    /// System register Z22
    Z22 = 0x4d9,
    /// System register Z23
    Z23 = 0x4da,
    /// System register Z24
    Z24 = 0x4db,
    /// System register Z25
    Z25 = 0x4dc,
    /// System register Z26
    Z26 = 0x4dd,
    /// System register Z27
    Z27 = 0x4de,
    /// System register Z28
    Z28 = 0x4df,
    /// System register Z29
    Z29 = 0x4e0,
    /// System register Z30
    Z30 = 0x4e1,
    /// System register Z31
    Z31 = 0x4e2,
    /// System register P0
    P0 = 0x4e3,
    /// System register P1
    P1 = 0x4e4,
    /// System register P2
    P2 = 0x4e5,
    /// System register P3
    P3 = 0x4e6,
    /// System register P4
    P4 = 0x4e7,
    /// System register P5
    P5 = 0x4e8,
    /// System register P6
    P6 = 0x4e9,
    /// System register P7
    P7 = 0x4ea,
    /// System register P8
    P8 = 0x4eb,
    /// System register P9
    P9 = 0x4ec,
    /// System register P10
    P10 = 0x4ed,
    /// System register P11
    P11 = 0x4ee,
    /// System register P12
    P12 = 0x4ef,
    /// System register P13
    P13 = 0x4f0,
    /// System register P14
    P14 = 0x4f1,
    /// System register P15
    P15 = 0x4f2,
    /// System register P16
    P16 = 0x4f3,
    /// System register P17
    P17 = 0x4f4,
    /// System register P18
    P18 = 0x4f5,
    /// System register P19
    P19 = 0x4f6,
    /// System register P20
    P20 = 0x4f7,
    /// System register P21
    P21 = 0x4f8,
    /// System register P22
    P22 = 0x4f9,
    /// System register P23
    P23 = 0x4fa,
    /// System register P24
    P24 = 0x4fb,
    /// System register P25
    P25 = 0x4fc,
    /// System register P26
    P26 = 0x4fd,
    /// System register P27
    P27 = 0x4fe,
    /// System register P28
    P28 = 0x4ff,
    /// System register P29
    P29 = 0x500,
    /// System register P30
    P30 = 0x501,
    /// System register P31
    P31 = 0x502,
    /// System register PF0
    PF0 = 0x503,
    /// System register PF1
    PF1 = 0x504,
    /// System register PF2
    PF2 = 0x505,
    /// System register PF3
    PF3 = 0x506,
    /// System register PF4
    PF4 = 0x507,
    /// System register PF5
    PF5 = 0x508,
    /// System register PF6
    PF6 = 0x509,
    /// System register PF7
    PF7 = 0x50a,
    /// System register PF8
    PF8 = 0x50b,
    /// System register PF9
    PF9 = 0x50c,
    /// System register PF10
    PF10 = 0x50d,
    /// System register PF11
    PF11 = 0x50e,
    /// System register PF12
    PF12 = 0x50f,
    /// System register PF13
    PF13 = 0x510,
    /// System register PF14
    PF14 = 0x511,
    /// System register PF15
    PF15 = 0x512,
    /// System register PF16
    PF16 = 0x513,
    /// System register PF17
    PF17 = 0x514,
    /// System register PF18
    PF18 = 0x515,
    /// System register PF19
    PF19 = 0x516,
    /// System register PF20
    PF20 = 0x517,
    /// System register PF21
    PF21 = 0x518,
    /// System register PF22
    PF22 = 0x519,
    /// System register PF23
    PF23 = 0x51a,
    /// System register PF24
    PF24 = 0x51b,
    /// System register PF25
    PF25 = 0x51c,
    /// System register PF26
    PF26 = 0x51d,
    /// System register PF27
    PF27 = 0x51e,
    /// System register PF28
    PF28 = 0x51f,
    /// System register PF29
    PF29 = 0x520,
    /// System register PF30
    PF30 = 0x521,
    /// System register PF31
    PF31 = 0x522,
    /// System register END
    END = 0x523,
}

impl Display for RegistersType {
    /// Print register name
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        match self {
            RegistersType::NONE => write!(f, "NONE"),
            RegistersType::W0 => write!(f, "W0"),
            RegistersType::W1 => write!(f, "W1"),
            RegistersType::W2 => write!(f, "W2"),
            RegistersType::W3 => write!(f, "W3"),
            RegistersType::W4 => write!(f, "W4"),
            RegistersType::W5 => write!(f, "W5"),
            RegistersType::W6 => write!(f, "W6"),
            RegistersType::W7 => write!(f, "W7"),
            RegistersType::W8 => write!(f, "W8"),
            RegistersType::W9 => write!(f, "W9"),
            RegistersType::W10 => write!(f, "W10"),
            RegistersType::W11 => write!(f, "W11"),
            RegistersType::W12 => write!(f, "W12"),
            RegistersType::W13 => write!(f, "W13"),
            RegistersType::W14 => write!(f, "W14"),
            RegistersType::W15 => write!(f, "W15"),
            RegistersType::W16 => write!(f, "W16"),
            RegistersType::W17 => write!(f, "W17"),
            RegistersType::W18 => write!(f, "W18"),
            RegistersType::W19 => write!(f, "W19"),
            RegistersType::W20 => write!(f, "W20"),
            RegistersType::W21 => write!(f, "W21"),
            RegistersType::W22 => write!(f, "W22"),
            RegistersType::W23 => write!(f, "W23"),
            RegistersType::W24 => write!(f, "W24"),
            RegistersType::W25 => write!(f, "W25"),
            RegistersType::W26 => write!(f, "W26"),
            RegistersType::W27 => write!(f, "W27"),
            RegistersType::W28 => write!(f, "W28"),
            RegistersType::W29 => write!(f, "W29"),
            RegistersType::W30 => write!(f, "W30"),
            RegistersType::WZR => write!(f, "WZR"),
            RegistersType::WSP => write!(f, "WSP"),
            RegistersType::X0 => write!(f, "X0"),
            RegistersType::X1 => write!(f, "X1"),
            RegistersType::X2 => write!(f, "X2"),
            RegistersType::X3 => write!(f, "X3"),
            RegistersType::X4 => write!(f, "X4"),
            RegistersType::X5 => write!(f, "X5"),
            RegistersType::X6 => write!(f, "X6"),
            RegistersType::X7 => write!(f, "X7"),
            RegistersType::X8 => write!(f, "X8"),
            RegistersType::X9 => write!(f, "X9"),
            RegistersType::X10 => write!(f, "X10"),
            RegistersType::X11 => write!(f, "X11"),
            RegistersType::X12 => write!(f, "X12"),
            RegistersType::X13 => write!(f, "X13"),
            RegistersType::X14 => write!(f, "X14"),
            RegistersType::X15 => write!(f, "X15"),
            RegistersType::X16 => write!(f, "X16"),
            RegistersType::X17 => write!(f, "X17"),
            RegistersType::X18 => write!(f, "X18"),
            RegistersType::X19 => write!(f, "X19"),
            RegistersType::X20 => write!(f, "X20"),
            RegistersType::X21 => write!(f, "X21"),
            RegistersType::X22 => write!(f, "X22"),
            RegistersType::X23 => write!(f, "X23"),
            RegistersType::X24 => write!(f, "X24"),
            RegistersType::X25 => write!(f, "X25"),
            RegistersType::X26 => write!(f, "X26"),
            RegistersType::X27 => write!(f, "X27"),
            RegistersType::X28 => write!(f, "X28"),
            RegistersType::X29 => write!(f, "X29"),
            RegistersType::X30 => write!(f, "X30"),
            RegistersType::XZR => write!(f, "XZR"),
            RegistersType::SP => write!(f, "SP"),
            RegistersType::V0 => write!(f, "V0"),
            RegistersType::V1 => write!(f, "V1"),
            RegistersType::V2 => write!(f, "V2"),
            RegistersType::V3 => write!(f, "V3"),
            RegistersType::V4 => write!(f, "V4"),
            RegistersType::V5 => write!(f, "V5"),
            RegistersType::V6 => write!(f, "V6"),
            RegistersType::V7 => write!(f, "V7"),
            RegistersType::V8 => write!(f, "V8"),
            RegistersType::V9 => write!(f, "V9"),
            RegistersType::V10 => write!(f, "V10"),
            RegistersType::V11 => write!(f, "V11"),
            RegistersType::V12 => write!(f, "V12"),
            RegistersType::V13 => write!(f, "V13"),
            RegistersType::V14 => write!(f, "V14"),
            RegistersType::V15 => write!(f, "V15"),
            RegistersType::V16 => write!(f, "V16"),
            RegistersType::V17 => write!(f, "V17"),
            RegistersType::V18 => write!(f, "V18"),
            RegistersType::V19 => write!(f, "V19"),
            RegistersType::V20 => write!(f, "V20"),
            RegistersType::V21 => write!(f, "V21"),
            RegistersType::V22 => write!(f, "V22"),
            RegistersType::V23 => write!(f, "V23"),
            RegistersType::V24 => write!(f, "V24"),
            RegistersType::V25 => write!(f, "V25"),
            RegistersType::V26 => write!(f, "V26"),
            RegistersType::V27 => write!(f, "V27"),
            RegistersType::V28 => write!(f, "V28"),
            RegistersType::V29 => write!(f, "V29"),
            RegistersType::V30 => write!(f, "V30"),
            RegistersType::V31 => write!(f, "V31"),
            RegistersType::B0 => write!(f, "B0"),
            RegistersType::B1 => write!(f, "B1"),
            RegistersType::B2 => write!(f, "B2"),
            RegistersType::B3 => write!(f, "B3"),
            RegistersType::B4 => write!(f, "B4"),
            RegistersType::B5 => write!(f, "B5"),
            RegistersType::B6 => write!(f, "B6"),
            RegistersType::B7 => write!(f, "B7"),
            RegistersType::B8 => write!(f, "B8"),
            RegistersType::B9 => write!(f, "B9"),
            RegistersType::B10 => write!(f, "B10"),
            RegistersType::B11 => write!(f, "B11"),
            RegistersType::B12 => write!(f, "B12"),
            RegistersType::B13 => write!(f, "B13"),
            RegistersType::B14 => write!(f, "B14"),
            RegistersType::B15 => write!(f, "B15"),
            RegistersType::B16 => write!(f, "B16"),
            RegistersType::B17 => write!(f, "B17"),
            RegistersType::B18 => write!(f, "B18"),
            RegistersType::B19 => write!(f, "B19"),
            RegistersType::B20 => write!(f, "B20"),
            RegistersType::B21 => write!(f, "B21"),
            RegistersType::B22 => write!(f, "B22"),
            RegistersType::B23 => write!(f, "B23"),
            RegistersType::B24 => write!(f, "B24"),
            RegistersType::B25 => write!(f, "B25"),
            RegistersType::B26 => write!(f, "B26"),
            RegistersType::B27 => write!(f, "B27"),
            RegistersType::B28 => write!(f, "B28"),
            RegistersType::B29 => write!(f, "B29"),
            RegistersType::B30 => write!(f, "B30"),
            RegistersType::B31 => write!(f, "B31"),
            RegistersType::H0 => write!(f, "H0"),
            RegistersType::H1 => write!(f, "H1"),
            RegistersType::H2 => write!(f, "H2"),
            RegistersType::H3 => write!(f, "H3"),
            RegistersType::H4 => write!(f, "H4"),
            RegistersType::H5 => write!(f, "H5"),
            RegistersType::H6 => write!(f, "H6"),
            RegistersType::H7 => write!(f, "H7"),
            RegistersType::H8 => write!(f, "H8"),
            RegistersType::H9 => write!(f, "H9"),
            RegistersType::H10 => write!(f, "H10"),
            RegistersType::H11 => write!(f, "H11"),
            RegistersType::H12 => write!(f, "H12"),
            RegistersType::H13 => write!(f, "H13"),
            RegistersType::H14 => write!(f, "H14"),
            RegistersType::H15 => write!(f, "H15"),
            RegistersType::H16 => write!(f, "H16"),
            RegistersType::H17 => write!(f, "H17"),
            RegistersType::H18 => write!(f, "H18"),
            RegistersType::H19 => write!(f, "H19"),
            RegistersType::H20 => write!(f, "H20"),
            RegistersType::H21 => write!(f, "H21"),
            RegistersType::H22 => write!(f, "H22"),
            RegistersType::H23 => write!(f, "H23"),
            RegistersType::H24 => write!(f, "H24"),
            RegistersType::H25 => write!(f, "H25"),
            RegistersType::H26 => write!(f, "H26"),
            RegistersType::H27 => write!(f, "H27"),
            RegistersType::H28 => write!(f, "H28"),
            RegistersType::H29 => write!(f, "H29"),
            RegistersType::H30 => write!(f, "H30"),
            RegistersType::H31 => write!(f, "H31"),
            RegistersType::S0 => write!(f, "S0"),
            RegistersType::S1 => write!(f, "S1"),
            RegistersType::S2 => write!(f, "S2"),
            RegistersType::S3 => write!(f, "S3"),
            RegistersType::S4 => write!(f, "S4"),
            RegistersType::S5 => write!(f, "S5"),
            RegistersType::S6 => write!(f, "S6"),
            RegistersType::S7 => write!(f, "S7"),
            RegistersType::S8 => write!(f, "S8"),
            RegistersType::S9 => write!(f, "S9"),
            RegistersType::S10 => write!(f, "S10"),
            RegistersType::S11 => write!(f, "S11"),
            RegistersType::S12 => write!(f, "S12"),
            RegistersType::S13 => write!(f, "S13"),
            RegistersType::S14 => write!(f, "S14"),
            RegistersType::S15 => write!(f, "S15"),
            RegistersType::S16 => write!(f, "S16"),
            RegistersType::S17 => write!(f, "S17"),
            RegistersType::S18 => write!(f, "S18"),
            RegistersType::S19 => write!(f, "S19"),
            RegistersType::S20 => write!(f, "S20"),
            RegistersType::S21 => write!(f, "S21"),
            RegistersType::S22 => write!(f, "S22"),
            RegistersType::S23 => write!(f, "S23"),
            RegistersType::S24 => write!(f, "S24"),
            RegistersType::S25 => write!(f, "S25"),
            RegistersType::S26 => write!(f, "S26"),
            RegistersType::S27 => write!(f, "S27"),
            RegistersType::S28 => write!(f, "S28"),
            RegistersType::S29 => write!(f, "S29"),
            RegistersType::S30 => write!(f, "S30"),
            RegistersType::S31 => write!(f, "S31"),
            RegistersType::D0 => write!(f, "D0"),
            RegistersType::D1 => write!(f, "D1"),
            RegistersType::D2 => write!(f, "D2"),
            RegistersType::D3 => write!(f, "D3"),
            RegistersType::D4 => write!(f, "D4"),
            RegistersType::D5 => write!(f, "D5"),
            RegistersType::D6 => write!(f, "D6"),
            RegistersType::D7 => write!(f, "D7"),
            RegistersType::D8 => write!(f, "D8"),
            RegistersType::D9 => write!(f, "D9"),
            RegistersType::D10 => write!(f, "D10"),
            RegistersType::D11 => write!(f, "D11"),
            RegistersType::D12 => write!(f, "D12"),
            RegistersType::D13 => write!(f, "D13"),
            RegistersType::D14 => write!(f, "D14"),
            RegistersType::D15 => write!(f, "D15"),
            RegistersType::D16 => write!(f, "D16"),
            RegistersType::D17 => write!(f, "D17"),
            RegistersType::D18 => write!(f, "D18"),
            RegistersType::D19 => write!(f, "D19"),
            RegistersType::D20 => write!(f, "D20"),
            RegistersType::D21 => write!(f, "D21"),
            RegistersType::D22 => write!(f, "D22"),
            RegistersType::D23 => write!(f, "D23"),
            RegistersType::D24 => write!(f, "D24"),
            RegistersType::D25 => write!(f, "D25"),
            RegistersType::D26 => write!(f, "D26"),
            RegistersType::D27 => write!(f, "D27"),
            RegistersType::D28 => write!(f, "D28"),
            RegistersType::D29 => write!(f, "D29"),
            RegistersType::D30 => write!(f, "D30"),
            RegistersType::D31 => write!(f, "D31"),
            RegistersType::Q0 => write!(f, "Q0"),
            RegistersType::Q1 => write!(f, "Q1"),
            RegistersType::Q2 => write!(f, "Q2"),
            RegistersType::Q3 => write!(f, "Q3"),
            RegistersType::Q4 => write!(f, "Q4"),
            RegistersType::Q5 => write!(f, "Q5"),
            RegistersType::Q6 => write!(f, "Q6"),
            RegistersType::Q7 => write!(f, "Q7"),
            RegistersType::Q8 => write!(f, "Q8"),
            RegistersType::Q9 => write!(f, "Q9"),
            RegistersType::Q10 => write!(f, "Q10"),
            RegistersType::Q11 => write!(f, "Q11"),
            RegistersType::Q12 => write!(f, "Q12"),
            RegistersType::Q13 => write!(f, "Q13"),
            RegistersType::Q14 => write!(f, "Q14"),
            RegistersType::Q15 => write!(f, "Q15"),
            RegistersType::Q16 => write!(f, "Q16"),
            RegistersType::Q17 => write!(f, "Q17"),
            RegistersType::Q18 => write!(f, "Q18"),
            RegistersType::Q19 => write!(f, "Q19"),
            RegistersType::Q20 => write!(f, "Q20"),
            RegistersType::Q21 => write!(f, "Q21"),
            RegistersType::Q22 => write!(f, "Q22"),
            RegistersType::Q23 => write!(f, "Q23"),
            RegistersType::Q24 => write!(f, "Q24"),
            RegistersType::Q25 => write!(f, "Q25"),
            RegistersType::Q26 => write!(f, "Q26"),
            RegistersType::Q27 => write!(f, "Q27"),
            RegistersType::Q28 => write!(f, "Q28"),
            RegistersType::Q29 => write!(f, "Q29"),
            RegistersType::Q30 => write!(f, "Q30"),
            RegistersType::Q31 => write!(f, "Q31"),
            RegistersType::V0_B0 => write!(f, "V0_B0"),
            RegistersType::V0_B1 => write!(f, "V0_B1"),
            RegistersType::V0_B2 => write!(f, "V0_B2"),
            RegistersType::V0_B3 => write!(f, "V0_B3"),
            RegistersType::V0_B4 => write!(f, "V0_B4"),
            RegistersType::V0_B5 => write!(f, "V0_B5"),
            RegistersType::V0_B6 => write!(f, "V0_B6"),
            RegistersType::V0_B7 => write!(f, "V0_B7"),
            RegistersType::V0_B8 => write!(f, "V0_B8"),
            RegistersType::V0_B9 => write!(f, "V0_B9"),
            RegistersType::V0_B10 => write!(f, "V0_B10"),
            RegistersType::V0_B11 => write!(f, "V0_B11"),
            RegistersType::V0_B12 => write!(f, "V0_B12"),
            RegistersType::V0_B13 => write!(f, "V0_B13"),
            RegistersType::V0_B14 => write!(f, "V0_B14"),
            RegistersType::V0_B15 => write!(f, "V0_B15"),
            RegistersType::V1_B0 => write!(f, "V1_B0"),
            RegistersType::V1_B1 => write!(f, "V1_B1"),
            RegistersType::V1_B2 => write!(f, "V1_B2"),
            RegistersType::V1_B3 => write!(f, "V1_B3"),
            RegistersType::V1_B4 => write!(f, "V1_B4"),
            RegistersType::V1_B5 => write!(f, "V1_B5"),
            RegistersType::V1_B6 => write!(f, "V1_B6"),
            RegistersType::V1_B7 => write!(f, "V1_B7"),
            RegistersType::V1_B8 => write!(f, "V1_B8"),
            RegistersType::V1_B9 => write!(f, "V1_B9"),
            RegistersType::V1_B10 => write!(f, "V1_B10"),
            RegistersType::V1_B11 => write!(f, "V1_B11"),
            RegistersType::V1_B12 => write!(f, "V1_B12"),
            RegistersType::V1_B13 => write!(f, "V1_B13"),
            RegistersType::V1_B14 => write!(f, "V1_B14"),
            RegistersType::V1_B15 => write!(f, "V1_B15"),
            RegistersType::V2_B0 => write!(f, "V2_B0"),
            RegistersType::V2_B1 => write!(f, "V2_B1"),
            RegistersType::V2_B2 => write!(f, "V2_B2"),
            RegistersType::V2_B3 => write!(f, "V2_B3"),
            RegistersType::V2_B4 => write!(f, "V2_B4"),
            RegistersType::V2_B5 => write!(f, "V2_B5"),
            RegistersType::V2_B6 => write!(f, "V2_B6"),
            RegistersType::V2_B7 => write!(f, "V2_B7"),
            RegistersType::V2_B8 => write!(f, "V2_B8"),
            RegistersType::V2_B9 => write!(f, "V2_B9"),
            RegistersType::V2_B10 => write!(f, "V2_B10"),
            RegistersType::V2_B11 => write!(f, "V2_B11"),
            RegistersType::V2_B12 => write!(f, "V2_B12"),
            RegistersType::V2_B13 => write!(f, "V2_B13"),
            RegistersType::V2_B14 => write!(f, "V2_B14"),
            RegistersType::V2_B15 => write!(f, "V2_B15"),
            RegistersType::V3_B0 => write!(f, "V3_B0"),
            RegistersType::V3_B1 => write!(f, "V3_B1"),
            RegistersType::V3_B2 => write!(f, "V3_B2"),
            RegistersType::V3_B3 => write!(f, "V3_B3"),
            RegistersType::V3_B4 => write!(f, "V3_B4"),
            RegistersType::V3_B5 => write!(f, "V3_B5"),
            RegistersType::V3_B6 => write!(f, "V3_B6"),
            RegistersType::V3_B7 => write!(f, "V3_B7"),
            RegistersType::V3_B8 => write!(f, "V3_B8"),
            RegistersType::V3_B9 => write!(f, "V3_B9"),
            RegistersType::V3_B10 => write!(f, "V3_B10"),
            RegistersType::V3_B11 => write!(f, "V3_B11"),
            RegistersType::V3_B12 => write!(f, "V3_B12"),
            RegistersType::V3_B13 => write!(f, "V3_B13"),
            RegistersType::V3_B14 => write!(f, "V3_B14"),
            RegistersType::V3_B15 => write!(f, "V3_B15"),
            RegistersType::V4_B0 => write!(f, "V4_B0"),
            RegistersType::V4_B1 => write!(f, "V4_B1"),
            RegistersType::V4_B2 => write!(f, "V4_B2"),
            RegistersType::V4_B3 => write!(f, "V4_B3"),
            RegistersType::V4_B4 => write!(f, "V4_B4"),
            RegistersType::V4_B5 => write!(f, "V4_B5"),
            RegistersType::V4_B6 => write!(f, "V4_B6"),
            RegistersType::V4_B7 => write!(f, "V4_B7"),
            RegistersType::V4_B8 => write!(f, "V4_B8"),
            RegistersType::V4_B9 => write!(f, "V4_B9"),
            RegistersType::V4_B10 => write!(f, "V4_B10"),
            RegistersType::V4_B11 => write!(f, "V4_B11"),
            RegistersType::V4_B12 => write!(f, "V4_B12"),
            RegistersType::V4_B13 => write!(f, "V4_B13"),
            RegistersType::V4_B14 => write!(f, "V4_B14"),
            RegistersType::V4_B15 => write!(f, "V4_B15"),
            RegistersType::V5_B0 => write!(f, "V5_B0"),
            RegistersType::V5_B1 => write!(f, "V5_B1"),
            RegistersType::V5_B2 => write!(f, "V5_B2"),
            RegistersType::V5_B3 => write!(f, "V5_B3"),
            RegistersType::V5_B4 => write!(f, "V5_B4"),
            RegistersType::V5_B5 => write!(f, "V5_B5"),
            RegistersType::V5_B6 => write!(f, "V5_B6"),
            RegistersType::V5_B7 => write!(f, "V5_B7"),
            RegistersType::V5_B8 => write!(f, "V5_B8"),
            RegistersType::V5_B9 => write!(f, "V5_B9"),
            RegistersType::V5_B10 => write!(f, "V5_B10"),
            RegistersType::V5_B11 => write!(f, "V5_B11"),
            RegistersType::V5_B12 => write!(f, "V5_B12"),
            RegistersType::V5_B13 => write!(f, "V5_B13"),
            RegistersType::V5_B14 => write!(f, "V5_B14"),
            RegistersType::V5_B15 => write!(f, "V5_B15"),
            RegistersType::V6_B0 => write!(f, "V6_B0"),
            RegistersType::V6_B1 => write!(f, "V6_B1"),
            RegistersType::V6_B2 => write!(f, "V6_B2"),
            RegistersType::V6_B3 => write!(f, "V6_B3"),
            RegistersType::V6_B4 => write!(f, "V6_B4"),
            RegistersType::V6_B5 => write!(f, "V6_B5"),
            RegistersType::V6_B6 => write!(f, "V6_B6"),
            RegistersType::V6_B7 => write!(f, "V6_B7"),
            RegistersType::V6_B8 => write!(f, "V6_B8"),
            RegistersType::V6_B9 => write!(f, "V6_B9"),
            RegistersType::V6_B10 => write!(f, "V6_B10"),
            RegistersType::V6_B11 => write!(f, "V6_B11"),
            RegistersType::V6_B12 => write!(f, "V6_B12"),
            RegistersType::V6_B13 => write!(f, "V6_B13"),
            RegistersType::V6_B14 => write!(f, "V6_B14"),
            RegistersType::V6_B15 => write!(f, "V6_B15"),
            RegistersType::V7_B0 => write!(f, "V7_B0"),
            RegistersType::V7_B1 => write!(f, "V7_B1"),
            RegistersType::V7_B2 => write!(f, "V7_B2"),
            RegistersType::V7_B3 => write!(f, "V7_B3"),
            RegistersType::V7_B4 => write!(f, "V7_B4"),
            RegistersType::V7_B5 => write!(f, "V7_B5"),
            RegistersType::V7_B6 => write!(f, "V7_B6"),
            RegistersType::V7_B7 => write!(f, "V7_B7"),
            RegistersType::V7_B8 => write!(f, "V7_B8"),
            RegistersType::V7_B9 => write!(f, "V7_B9"),
            RegistersType::V7_B10 => write!(f, "V7_B10"),
            RegistersType::V7_B11 => write!(f, "V7_B11"),
            RegistersType::V7_B12 => write!(f, "V7_B12"),
            RegistersType::V7_B13 => write!(f, "V7_B13"),
            RegistersType::V7_B14 => write!(f, "V7_B14"),
            RegistersType::V7_B15 => write!(f, "V7_B15"),
            RegistersType::V8_B0 => write!(f, "V8_B0"),
            RegistersType::V8_B1 => write!(f, "V8_B1"),
            RegistersType::V8_B2 => write!(f, "V8_B2"),
            RegistersType::V8_B3 => write!(f, "V8_B3"),
            RegistersType::V8_B4 => write!(f, "V8_B4"),
            RegistersType::V8_B5 => write!(f, "V8_B5"),
            RegistersType::V8_B6 => write!(f, "V8_B6"),
            RegistersType::V8_B7 => write!(f, "V8_B7"),
            RegistersType::V8_B8 => write!(f, "V8_B8"),
            RegistersType::V8_B9 => write!(f, "V8_B9"),
            RegistersType::V8_B10 => write!(f, "V8_B10"),
            RegistersType::V8_B11 => write!(f, "V8_B11"),
            RegistersType::V8_B12 => write!(f, "V8_B12"),
            RegistersType::V8_B13 => write!(f, "V8_B13"),
            RegistersType::V8_B14 => write!(f, "V8_B14"),
            RegistersType::V8_B15 => write!(f, "V8_B15"),
            RegistersType::V9_B0 => write!(f, "V9_B0"),
            RegistersType::V9_B1 => write!(f, "V9_B1"),
            RegistersType::V9_B2 => write!(f, "V9_B2"),
            RegistersType::V9_B3 => write!(f, "V9_B3"),
            RegistersType::V9_B4 => write!(f, "V9_B4"),
            RegistersType::V9_B5 => write!(f, "V9_B5"),
            RegistersType::V9_B6 => write!(f, "V9_B6"),
            RegistersType::V9_B7 => write!(f, "V9_B7"),
            RegistersType::V9_B8 => write!(f, "V9_B8"),
            RegistersType::V9_B9 => write!(f, "V9_B9"),
            RegistersType::V9_B10 => write!(f, "V9_B10"),
            RegistersType::V9_B11 => write!(f, "V9_B11"),
            RegistersType::V9_B12 => write!(f, "V9_B12"),
            RegistersType::V9_B13 => write!(f, "V9_B13"),
            RegistersType::V9_B14 => write!(f, "V9_B14"),
            RegistersType::V9_B15 => write!(f, "V9_B15"),
            RegistersType::V10_B0 => write!(f, "V10_B0"),
            RegistersType::V10_B1 => write!(f, "V10_B1"),
            RegistersType::V10_B2 => write!(f, "V10_B2"),
            RegistersType::V10_B3 => write!(f, "V10_B3"),
            RegistersType::V10_B4 => write!(f, "V10_B4"),
            RegistersType::V10_B5 => write!(f, "V10_B5"),
            RegistersType::V10_B6 => write!(f, "V10_B6"),
            RegistersType::V10_B7 => write!(f, "V10_B7"),
            RegistersType::V10_B8 => write!(f, "V10_B8"),
            RegistersType::V10_B9 => write!(f, "V10_B9"),
            RegistersType::V10_B10 => write!(f, "V10_B10"),
            RegistersType::V10_B11 => write!(f, "V10_B11"),
            RegistersType::V10_B12 => write!(f, "V10_B12"),
            RegistersType::V10_B13 => write!(f, "V10_B13"),
            RegistersType::V10_B14 => write!(f, "V10_B14"),
            RegistersType::V10_B15 => write!(f, "V10_B15"),
            RegistersType::V11_B0 => write!(f, "V11_B0"),
            RegistersType::V11_B1 => write!(f, "V11_B1"),
            RegistersType::V11_B2 => write!(f, "V11_B2"),
            RegistersType::V11_B3 => write!(f, "V11_B3"),
            RegistersType::V11_B4 => write!(f, "V11_B4"),
            RegistersType::V11_B5 => write!(f, "V11_B5"),
            RegistersType::V11_B6 => write!(f, "V11_B6"),
            RegistersType::V11_B7 => write!(f, "V11_B7"),
            RegistersType::V11_B8 => write!(f, "V11_B8"),
            RegistersType::V11_B9 => write!(f, "V11_B9"),
            RegistersType::V11_B10 => write!(f, "V11_B10"),
            RegistersType::V11_B11 => write!(f, "V11_B11"),
            RegistersType::V11_B12 => write!(f, "V11_B12"),
            RegistersType::V11_B13 => write!(f, "V11_B13"),
            RegistersType::V11_B14 => write!(f, "V11_B14"),
            RegistersType::V11_B15 => write!(f, "V11_B15"),
            RegistersType::V12_B0 => write!(f, "V12_B0"),
            RegistersType::V12_B1 => write!(f, "V12_B1"),
            RegistersType::V12_B2 => write!(f, "V12_B2"),
            RegistersType::V12_B3 => write!(f, "V12_B3"),
            RegistersType::V12_B4 => write!(f, "V12_B4"),
            RegistersType::V12_B5 => write!(f, "V12_B5"),
            RegistersType::V12_B6 => write!(f, "V12_B6"),
            RegistersType::V12_B7 => write!(f, "V12_B7"),
            RegistersType::V12_B8 => write!(f, "V12_B8"),
            RegistersType::V12_B9 => write!(f, "V12_B9"),
            RegistersType::V12_B10 => write!(f, "V12_B10"),
            RegistersType::V12_B11 => write!(f, "V12_B11"),
            RegistersType::V12_B12 => write!(f, "V12_B12"),
            RegistersType::V12_B13 => write!(f, "V12_B13"),
            RegistersType::V12_B14 => write!(f, "V12_B14"),
            RegistersType::V12_B15 => write!(f, "V12_B15"),
            RegistersType::V13_B0 => write!(f, "V13_B0"),
            RegistersType::V13_B1 => write!(f, "V13_B1"),
            RegistersType::V13_B2 => write!(f, "V13_B2"),
            RegistersType::V13_B3 => write!(f, "V13_B3"),
            RegistersType::V13_B4 => write!(f, "V13_B4"),
            RegistersType::V13_B5 => write!(f, "V13_B5"),
            RegistersType::V13_B6 => write!(f, "V13_B6"),
            RegistersType::V13_B7 => write!(f, "V13_B7"),
            RegistersType::V13_B8 => write!(f, "V13_B8"),
            RegistersType::V13_B9 => write!(f, "V13_B9"),
            RegistersType::V13_B10 => write!(f, "V13_B10"),
            RegistersType::V13_B11 => write!(f, "V13_B11"),
            RegistersType::V13_B12 => write!(f, "V13_B12"),
            RegistersType::V13_B13 => write!(f, "V13_B13"),
            RegistersType::V13_B14 => write!(f, "V13_B14"),
            RegistersType::V13_B15 => write!(f, "V13_B15"),
            RegistersType::V14_B0 => write!(f, "V14_B0"),
            RegistersType::V14_B1 => write!(f, "V14_B1"),
            RegistersType::V14_B2 => write!(f, "V14_B2"),
            RegistersType::V14_B3 => write!(f, "V14_B3"),
            RegistersType::V14_B4 => write!(f, "V14_B4"),
            RegistersType::V14_B5 => write!(f, "V14_B5"),
            RegistersType::V14_B6 => write!(f, "V14_B6"),
            RegistersType::V14_B7 => write!(f, "V14_B7"),
            RegistersType::V14_B8 => write!(f, "V14_B8"),
            RegistersType::V14_B9 => write!(f, "V14_B9"),
            RegistersType::V14_B10 => write!(f, "V14_B10"),
            RegistersType::V14_B11 => write!(f, "V14_B11"),
            RegistersType::V14_B12 => write!(f, "V14_B12"),
            RegistersType::V14_B13 => write!(f, "V14_B13"),
            RegistersType::V14_B14 => write!(f, "V14_B14"),
            RegistersType::V14_B15 => write!(f, "V14_B15"),
            RegistersType::V15_B0 => write!(f, "V15_B0"),
            RegistersType::V15_B1 => write!(f, "V15_B1"),
            RegistersType::V15_B2 => write!(f, "V15_B2"),
            RegistersType::V15_B3 => write!(f, "V15_B3"),
            RegistersType::V15_B4 => write!(f, "V15_B4"),
            RegistersType::V15_B5 => write!(f, "V15_B5"),
            RegistersType::V15_B6 => write!(f, "V15_B6"),
            RegistersType::V15_B7 => write!(f, "V15_B7"),
            RegistersType::V15_B8 => write!(f, "V15_B8"),
            RegistersType::V15_B9 => write!(f, "V15_B9"),
            RegistersType::V15_B10 => write!(f, "V15_B10"),
            RegistersType::V15_B11 => write!(f, "V15_B11"),
            RegistersType::V15_B12 => write!(f, "V15_B12"),
            RegistersType::V15_B13 => write!(f, "V15_B13"),
            RegistersType::V15_B14 => write!(f, "V15_B14"),
            RegistersType::V15_B15 => write!(f, "V15_B15"),
            RegistersType::V16_B0 => write!(f, "V16_B0"),
            RegistersType::V16_B1 => write!(f, "V16_B1"),
            RegistersType::V16_B2 => write!(f, "V16_B2"),
            RegistersType::V16_B3 => write!(f, "V16_B3"),
            RegistersType::V16_B4 => write!(f, "V16_B4"),
            RegistersType::V16_B5 => write!(f, "V16_B5"),
            RegistersType::V16_B6 => write!(f, "V16_B6"),
            RegistersType::V16_B7 => write!(f, "V16_B7"),
            RegistersType::V16_B8 => write!(f, "V16_B8"),
            RegistersType::V16_B9 => write!(f, "V16_B9"),
            RegistersType::V16_B10 => write!(f, "V16_B10"),
            RegistersType::V16_B11 => write!(f, "V16_B11"),
            RegistersType::V16_B12 => write!(f, "V16_B12"),
            RegistersType::V16_B13 => write!(f, "V16_B13"),
            RegistersType::V16_B14 => write!(f, "V16_B14"),
            RegistersType::V16_B15 => write!(f, "V16_B15"),
            RegistersType::V17_B0 => write!(f, "V17_B0"),
            RegistersType::V17_B1 => write!(f, "V17_B1"),
            RegistersType::V17_B2 => write!(f, "V17_B2"),
            RegistersType::V17_B3 => write!(f, "V17_B3"),
            RegistersType::V17_B4 => write!(f, "V17_B4"),
            RegistersType::V17_B5 => write!(f, "V17_B5"),
            RegistersType::V17_B6 => write!(f, "V17_B6"),
            RegistersType::V17_B7 => write!(f, "V17_B7"),
            RegistersType::V17_B8 => write!(f, "V17_B8"),
            RegistersType::V17_B9 => write!(f, "V17_B9"),
            RegistersType::V17_B10 => write!(f, "V17_B10"),
            RegistersType::V17_B11 => write!(f, "V17_B11"),
            RegistersType::V17_B12 => write!(f, "V17_B12"),
            RegistersType::V17_B13 => write!(f, "V17_B13"),
            RegistersType::V17_B14 => write!(f, "V17_B14"),
            RegistersType::V17_B15 => write!(f, "V17_B15"),
            RegistersType::V18_B0 => write!(f, "V18_B0"),
            RegistersType::V18_B1 => write!(f, "V18_B1"),
            RegistersType::V18_B2 => write!(f, "V18_B2"),
            RegistersType::V18_B3 => write!(f, "V18_B3"),
            RegistersType::V18_B4 => write!(f, "V18_B4"),
            RegistersType::V18_B5 => write!(f, "V18_B5"),
            RegistersType::V18_B6 => write!(f, "V18_B6"),
            RegistersType::V18_B7 => write!(f, "V18_B7"),
            RegistersType::V18_B8 => write!(f, "V18_B8"),
            RegistersType::V18_B9 => write!(f, "V18_B9"),
            RegistersType::V18_B10 => write!(f, "V18_B10"),
            RegistersType::V18_B11 => write!(f, "V18_B11"),
            RegistersType::V18_B12 => write!(f, "V18_B12"),
            RegistersType::V18_B13 => write!(f, "V18_B13"),
            RegistersType::V18_B14 => write!(f, "V18_B14"),
            RegistersType::V18_B15 => write!(f, "V18_B15"),
            RegistersType::V19_B0 => write!(f, "V19_B0"),
            RegistersType::V19_B1 => write!(f, "V19_B1"),
            RegistersType::V19_B2 => write!(f, "V19_B2"),
            RegistersType::V19_B3 => write!(f, "V19_B3"),
            RegistersType::V19_B4 => write!(f, "V19_B4"),
            RegistersType::V19_B5 => write!(f, "V19_B5"),
            RegistersType::V19_B6 => write!(f, "V19_B6"),
            RegistersType::V19_B7 => write!(f, "V19_B7"),
            RegistersType::V19_B8 => write!(f, "V19_B8"),
            RegistersType::V19_B9 => write!(f, "V19_B9"),
            RegistersType::V19_B10 => write!(f, "V19_B10"),
            RegistersType::V19_B11 => write!(f, "V19_B11"),
            RegistersType::V19_B12 => write!(f, "V19_B12"),
            RegistersType::V19_B13 => write!(f, "V19_B13"),
            RegistersType::V19_B14 => write!(f, "V19_B14"),
            RegistersType::V19_B15 => write!(f, "V19_B15"),
            RegistersType::V20_B0 => write!(f, "V20_B0"),
            RegistersType::V20_B1 => write!(f, "V20_B1"),
            RegistersType::V20_B2 => write!(f, "V20_B2"),
            RegistersType::V20_B3 => write!(f, "V20_B3"),
            RegistersType::V20_B4 => write!(f, "V20_B4"),
            RegistersType::V20_B5 => write!(f, "V20_B5"),
            RegistersType::V20_B6 => write!(f, "V20_B6"),
            RegistersType::V20_B7 => write!(f, "V20_B7"),
            RegistersType::V20_B8 => write!(f, "V20_B8"),
            RegistersType::V20_B9 => write!(f, "V20_B9"),
            RegistersType::V20_B10 => write!(f, "V20_B10"),
            RegistersType::V20_B11 => write!(f, "V20_B11"),
            RegistersType::V20_B12 => write!(f, "V20_B12"),
            RegistersType::V20_B13 => write!(f, "V20_B13"),
            RegistersType::V20_B14 => write!(f, "V20_B14"),
            RegistersType::V20_B15 => write!(f, "V20_B15"),
            RegistersType::V21_B0 => write!(f, "V21_B0"),
            RegistersType::V21_B1 => write!(f, "V21_B1"),
            RegistersType::V21_B2 => write!(f, "V21_B2"),
            RegistersType::V21_B3 => write!(f, "V21_B3"),
            RegistersType::V21_B4 => write!(f, "V21_B4"),
            RegistersType::V21_B5 => write!(f, "V21_B5"),
            RegistersType::V21_B6 => write!(f, "V21_B6"),
            RegistersType::V21_B7 => write!(f, "V21_B7"),
            RegistersType::V21_B8 => write!(f, "V21_B8"),
            RegistersType::V21_B9 => write!(f, "V21_B9"),
            RegistersType::V21_B10 => write!(f, "V21_B10"),
            RegistersType::V21_B11 => write!(f, "V21_B11"),
            RegistersType::V21_B12 => write!(f, "V21_B12"),
            RegistersType::V21_B13 => write!(f, "V21_B13"),
            RegistersType::V21_B14 => write!(f, "V21_B14"),
            RegistersType::V21_B15 => write!(f, "V21_B15"),
            RegistersType::V22_B0 => write!(f, "V22_B0"),
            RegistersType::V22_B1 => write!(f, "V22_B1"),
            RegistersType::V22_B2 => write!(f, "V22_B2"),
            RegistersType::V22_B3 => write!(f, "V22_B3"),
            RegistersType::V22_B4 => write!(f, "V22_B4"),
            RegistersType::V22_B5 => write!(f, "V22_B5"),
            RegistersType::V22_B6 => write!(f, "V22_B6"),
            RegistersType::V22_B7 => write!(f, "V22_B7"),
            RegistersType::V22_B8 => write!(f, "V22_B8"),
            RegistersType::V22_B9 => write!(f, "V22_B9"),
            RegistersType::V22_B10 => write!(f, "V22_B10"),
            RegistersType::V22_B11 => write!(f, "V22_B11"),
            RegistersType::V22_B12 => write!(f, "V22_B12"),
            RegistersType::V22_B13 => write!(f, "V22_B13"),
            RegistersType::V22_B14 => write!(f, "V22_B14"),
            RegistersType::V22_B15 => write!(f, "V22_B15"),
            RegistersType::V23_B0 => write!(f, "V23_B0"),
            RegistersType::V23_B1 => write!(f, "V23_B1"),
            RegistersType::V23_B2 => write!(f, "V23_B2"),
            RegistersType::V23_B3 => write!(f, "V23_B3"),
            RegistersType::V23_B4 => write!(f, "V23_B4"),
            RegistersType::V23_B5 => write!(f, "V23_B5"),
            RegistersType::V23_B6 => write!(f, "V23_B6"),
            RegistersType::V23_B7 => write!(f, "V23_B7"),
            RegistersType::V23_B8 => write!(f, "V23_B8"),
            RegistersType::V23_B9 => write!(f, "V23_B9"),
            RegistersType::V23_B10 => write!(f, "V23_B10"),
            RegistersType::V23_B11 => write!(f, "V23_B11"),
            RegistersType::V23_B12 => write!(f, "V23_B12"),
            RegistersType::V23_B13 => write!(f, "V23_B13"),
            RegistersType::V23_B14 => write!(f, "V23_B14"),
            RegistersType::V23_B15 => write!(f, "V23_B15"),
            RegistersType::V24_B0 => write!(f, "V24_B0"),
            RegistersType::V24_B1 => write!(f, "V24_B1"),
            RegistersType::V24_B2 => write!(f, "V24_B2"),
            RegistersType::V24_B3 => write!(f, "V24_B3"),
            RegistersType::V24_B4 => write!(f, "V24_B4"),
            RegistersType::V24_B5 => write!(f, "V24_B5"),
            RegistersType::V24_B6 => write!(f, "V24_B6"),
            RegistersType::V24_B7 => write!(f, "V24_B7"),
            RegistersType::V24_B8 => write!(f, "V24_B8"),
            RegistersType::V24_B9 => write!(f, "V24_B9"),
            RegistersType::V24_B10 => write!(f, "V24_B10"),
            RegistersType::V24_B11 => write!(f, "V24_B11"),
            RegistersType::V24_B12 => write!(f, "V24_B12"),
            RegistersType::V24_B13 => write!(f, "V24_B13"),
            RegistersType::V24_B14 => write!(f, "V24_B14"),
            RegistersType::V24_B15 => write!(f, "V24_B15"),
            RegistersType::V25_B0 => write!(f, "V25_B0"),
            RegistersType::V25_B1 => write!(f, "V25_B1"),
            RegistersType::V25_B2 => write!(f, "V25_B2"),
            RegistersType::V25_B3 => write!(f, "V25_B3"),
            RegistersType::V25_B4 => write!(f, "V25_B4"),
            RegistersType::V25_B5 => write!(f, "V25_B5"),
            RegistersType::V25_B6 => write!(f, "V25_B6"),
            RegistersType::V25_B7 => write!(f, "V25_B7"),
            RegistersType::V25_B8 => write!(f, "V25_B8"),
            RegistersType::V25_B9 => write!(f, "V25_B9"),
            RegistersType::V25_B10 => write!(f, "V25_B10"),
            RegistersType::V25_B11 => write!(f, "V25_B11"),
            RegistersType::V25_B12 => write!(f, "V25_B12"),
            RegistersType::V25_B13 => write!(f, "V25_B13"),
            RegistersType::V25_B14 => write!(f, "V25_B14"),
            RegistersType::V25_B15 => write!(f, "V25_B15"),
            RegistersType::V26_B0 => write!(f, "V26_B0"),
            RegistersType::V26_B1 => write!(f, "V26_B1"),
            RegistersType::V26_B2 => write!(f, "V26_B2"),
            RegistersType::V26_B3 => write!(f, "V26_B3"),
            RegistersType::V26_B4 => write!(f, "V26_B4"),
            RegistersType::V26_B5 => write!(f, "V26_B5"),
            RegistersType::V26_B6 => write!(f, "V26_B6"),
            RegistersType::V26_B7 => write!(f, "V26_B7"),
            RegistersType::V26_B8 => write!(f, "V26_B8"),
            RegistersType::V26_B9 => write!(f, "V26_B9"),
            RegistersType::V26_B10 => write!(f, "V26_B10"),
            RegistersType::V26_B11 => write!(f, "V26_B11"),
            RegistersType::V26_B12 => write!(f, "V26_B12"),
            RegistersType::V26_B13 => write!(f, "V26_B13"),
            RegistersType::V26_B14 => write!(f, "V26_B14"),
            RegistersType::V26_B15 => write!(f, "V26_B15"),
            RegistersType::V27_B0 => write!(f, "V27_B0"),
            RegistersType::V27_B1 => write!(f, "V27_B1"),
            RegistersType::V27_B2 => write!(f, "V27_B2"),
            RegistersType::V27_B3 => write!(f, "V27_B3"),
            RegistersType::V27_B4 => write!(f, "V27_B4"),
            RegistersType::V27_B5 => write!(f, "V27_B5"),
            RegistersType::V27_B6 => write!(f, "V27_B6"),
            RegistersType::V27_B7 => write!(f, "V27_B7"),
            RegistersType::V27_B8 => write!(f, "V27_B8"),
            RegistersType::V27_B9 => write!(f, "V27_B9"),
            RegistersType::V27_B10 => write!(f, "V27_B10"),
            RegistersType::V27_B11 => write!(f, "V27_B11"),
            RegistersType::V27_B12 => write!(f, "V27_B12"),
            RegistersType::V27_B13 => write!(f, "V27_B13"),
            RegistersType::V27_B14 => write!(f, "V27_B14"),
            RegistersType::V27_B15 => write!(f, "V27_B15"),
            RegistersType::V28_B0 => write!(f, "V28_B0"),
            RegistersType::V28_B1 => write!(f, "V28_B1"),
            RegistersType::V28_B2 => write!(f, "V28_B2"),
            RegistersType::V28_B3 => write!(f, "V28_B3"),
            RegistersType::V28_B4 => write!(f, "V28_B4"),
            RegistersType::V28_B5 => write!(f, "V28_B5"),
            RegistersType::V28_B6 => write!(f, "V28_B6"),
            RegistersType::V28_B7 => write!(f, "V28_B7"),
            RegistersType::V28_B8 => write!(f, "V28_B8"),
            RegistersType::V28_B9 => write!(f, "V28_B9"),
            RegistersType::V28_B10 => write!(f, "V28_B10"),
            RegistersType::V28_B11 => write!(f, "V28_B11"),
            RegistersType::V28_B12 => write!(f, "V28_B12"),
            RegistersType::V28_B13 => write!(f, "V28_B13"),
            RegistersType::V28_B14 => write!(f, "V28_B14"),
            RegistersType::V28_B15 => write!(f, "V28_B15"),
            RegistersType::V29_B0 => write!(f, "V29_B0"),
            RegistersType::V29_B1 => write!(f, "V29_B1"),
            RegistersType::V29_B2 => write!(f, "V29_B2"),
            RegistersType::V29_B3 => write!(f, "V29_B3"),
            RegistersType::V29_B4 => write!(f, "V29_B4"),
            RegistersType::V29_B5 => write!(f, "V29_B5"),
            RegistersType::V29_B6 => write!(f, "V29_B6"),
            RegistersType::V29_B7 => write!(f, "V29_B7"),
            RegistersType::V29_B8 => write!(f, "V29_B8"),
            RegistersType::V29_B9 => write!(f, "V29_B9"),
            RegistersType::V29_B10 => write!(f, "V29_B10"),
            RegistersType::V29_B11 => write!(f, "V29_B11"),
            RegistersType::V29_B12 => write!(f, "V29_B12"),
            RegistersType::V29_B13 => write!(f, "V29_B13"),
            RegistersType::V29_B14 => write!(f, "V29_B14"),
            RegistersType::V29_B15 => write!(f, "V29_B15"),
            RegistersType::V30_B0 => write!(f, "V30_B0"),
            RegistersType::V30_B1 => write!(f, "V30_B1"),
            RegistersType::V30_B2 => write!(f, "V30_B2"),
            RegistersType::V30_B3 => write!(f, "V30_B3"),
            RegistersType::V30_B4 => write!(f, "V30_B4"),
            RegistersType::V30_B5 => write!(f, "V30_B5"),
            RegistersType::V30_B6 => write!(f, "V30_B6"),
            RegistersType::V30_B7 => write!(f, "V30_B7"),
            RegistersType::V30_B8 => write!(f, "V30_B8"),
            RegistersType::V30_B9 => write!(f, "V30_B9"),
            RegistersType::V30_B10 => write!(f, "V30_B10"),
            RegistersType::V30_B11 => write!(f, "V30_B11"),
            RegistersType::V30_B12 => write!(f, "V30_B12"),
            RegistersType::V30_B13 => write!(f, "V30_B13"),
            RegistersType::V30_B14 => write!(f, "V30_B14"),
            RegistersType::V30_B15 => write!(f, "V30_B15"),
            RegistersType::V31_B0 => write!(f, "V31_B0"),
            RegistersType::V31_B1 => write!(f, "V31_B1"),
            RegistersType::V31_B2 => write!(f, "V31_B2"),
            RegistersType::V31_B3 => write!(f, "V31_B3"),
            RegistersType::V31_B4 => write!(f, "V31_B4"),
            RegistersType::V31_B5 => write!(f, "V31_B5"),
            RegistersType::V31_B6 => write!(f, "V31_B6"),
            RegistersType::V31_B7 => write!(f, "V31_B7"),
            RegistersType::V31_B8 => write!(f, "V31_B8"),
            RegistersType::V31_B9 => write!(f, "V31_B9"),
            RegistersType::V31_B10 => write!(f, "V31_B10"),
            RegistersType::V31_B11 => write!(f, "V31_B11"),
            RegistersType::V31_B12 => write!(f, "V31_B12"),
            RegistersType::V31_B13 => write!(f, "V31_B13"),
            RegistersType::V31_B14 => write!(f, "V31_B14"),
            RegistersType::V31_B15 => write!(f, "V31_B15"),
            RegistersType::V0_H0 => write!(f, "V0_H0"),
            RegistersType::V0_H1 => write!(f, "V0_H1"),
            RegistersType::V0_H2 => write!(f, "V0_H2"),
            RegistersType::V0_H3 => write!(f, "V0_H3"),
            RegistersType::V0_H4 => write!(f, "V0_H4"),
            RegistersType::V0_H5 => write!(f, "V0_H5"),
            RegistersType::V0_H6 => write!(f, "V0_H6"),
            RegistersType::V0_H7 => write!(f, "V0_H7"),
            RegistersType::V1_H0 => write!(f, "V1_H0"),
            RegistersType::V1_H1 => write!(f, "V1_H1"),
            RegistersType::V1_H2 => write!(f, "V1_H2"),
            RegistersType::V1_H3 => write!(f, "V1_H3"),
            RegistersType::V1_H4 => write!(f, "V1_H4"),
            RegistersType::V1_H5 => write!(f, "V1_H5"),
            RegistersType::V1_H6 => write!(f, "V1_H6"),
            RegistersType::V1_H7 => write!(f, "V1_H7"),
            RegistersType::V2_H0 => write!(f, "V2_H0"),
            RegistersType::V2_H1 => write!(f, "V2_H1"),
            RegistersType::V2_H2 => write!(f, "V2_H2"),
            RegistersType::V2_H3 => write!(f, "V2_H3"),
            RegistersType::V2_H4 => write!(f, "V2_H4"),
            RegistersType::V2_H5 => write!(f, "V2_H5"),
            RegistersType::V2_H6 => write!(f, "V2_H6"),
            RegistersType::V2_H7 => write!(f, "V2_H7"),
            RegistersType::V3_H0 => write!(f, "V3_H0"),
            RegistersType::V3_H1 => write!(f, "V3_H1"),
            RegistersType::V3_H2 => write!(f, "V3_H2"),
            RegistersType::V3_H3 => write!(f, "V3_H3"),
            RegistersType::V3_H4 => write!(f, "V3_H4"),
            RegistersType::V3_H5 => write!(f, "V3_H5"),
            RegistersType::V3_H6 => write!(f, "V3_H6"),
            RegistersType::V3_H7 => write!(f, "V3_H7"),
            RegistersType::V4_H0 => write!(f, "V4_H0"),
            RegistersType::V4_H1 => write!(f, "V4_H1"),
            RegistersType::V4_H2 => write!(f, "V4_H2"),
            RegistersType::V4_H3 => write!(f, "V4_H3"),
            RegistersType::V4_H4 => write!(f, "V4_H4"),
            RegistersType::V4_H5 => write!(f, "V4_H5"),
            RegistersType::V4_H6 => write!(f, "V4_H6"),
            RegistersType::V4_H7 => write!(f, "V4_H7"),
            RegistersType::V5_H0 => write!(f, "V5_H0"),
            RegistersType::V5_H1 => write!(f, "V5_H1"),
            RegistersType::V5_H2 => write!(f, "V5_H2"),
            RegistersType::V5_H3 => write!(f, "V5_H3"),
            RegistersType::V5_H4 => write!(f, "V5_H4"),
            RegistersType::V5_H5 => write!(f, "V5_H5"),
            RegistersType::V5_H6 => write!(f, "V5_H6"),
            RegistersType::V5_H7 => write!(f, "V5_H7"),
            RegistersType::V6_H0 => write!(f, "V6_H0"),
            RegistersType::V6_H1 => write!(f, "V6_H1"),
            RegistersType::V6_H2 => write!(f, "V6_H2"),
            RegistersType::V6_H3 => write!(f, "V6_H3"),
            RegistersType::V6_H4 => write!(f, "V6_H4"),
            RegistersType::V6_H5 => write!(f, "V6_H5"),
            RegistersType::V6_H6 => write!(f, "V6_H6"),
            RegistersType::V6_H7 => write!(f, "V6_H7"),
            RegistersType::V7_H0 => write!(f, "V7_H0"),
            RegistersType::V7_H1 => write!(f, "V7_H1"),
            RegistersType::V7_H2 => write!(f, "V7_H2"),
            RegistersType::V7_H3 => write!(f, "V7_H3"),
            RegistersType::V7_H4 => write!(f, "V7_H4"),
            RegistersType::V7_H5 => write!(f, "V7_H5"),
            RegistersType::V7_H6 => write!(f, "V7_H6"),
            RegistersType::V7_H7 => write!(f, "V7_H7"),
            RegistersType::V8_H0 => write!(f, "V8_H0"),
            RegistersType::V8_H1 => write!(f, "V8_H1"),
            RegistersType::V8_H2 => write!(f, "V8_H2"),
            RegistersType::V8_H3 => write!(f, "V8_H3"),
            RegistersType::V8_H4 => write!(f, "V8_H4"),
            RegistersType::V8_H5 => write!(f, "V8_H5"),
            RegistersType::V8_H6 => write!(f, "V8_H6"),
            RegistersType::V8_H7 => write!(f, "V8_H7"),
            RegistersType::V9_H0 => write!(f, "V9_H0"),
            RegistersType::V9_H1 => write!(f, "V9_H1"),
            RegistersType::V9_H2 => write!(f, "V9_H2"),
            RegistersType::V9_H3 => write!(f, "V9_H3"),
            RegistersType::V9_H4 => write!(f, "V9_H4"),
            RegistersType::V9_H5 => write!(f, "V9_H5"),
            RegistersType::V9_H6 => write!(f, "V9_H6"),
            RegistersType::V9_H7 => write!(f, "V9_H7"),
            RegistersType::V10_H0 => write!(f, "V10_H0"),
            RegistersType::V10_H1 => write!(f, "V10_H1"),
            RegistersType::V10_H2 => write!(f, "V10_H2"),
            RegistersType::V10_H3 => write!(f, "V10_H3"),
            RegistersType::V10_H4 => write!(f, "V10_H4"),
            RegistersType::V10_H5 => write!(f, "V10_H5"),
            RegistersType::V10_H6 => write!(f, "V10_H6"),
            RegistersType::V10_H7 => write!(f, "V10_H7"),
            RegistersType::V11_H0 => write!(f, "V11_H0"),
            RegistersType::V11_H1 => write!(f, "V11_H1"),
            RegistersType::V11_H2 => write!(f, "V11_H2"),
            RegistersType::V11_H3 => write!(f, "V11_H3"),
            RegistersType::V11_H4 => write!(f, "V11_H4"),
            RegistersType::V11_H5 => write!(f, "V11_H5"),
            RegistersType::V11_H6 => write!(f, "V11_H6"),
            RegistersType::V11_H7 => write!(f, "V11_H7"),
            RegistersType::V12_H0 => write!(f, "V12_H0"),
            RegistersType::V12_H1 => write!(f, "V12_H1"),
            RegistersType::V12_H2 => write!(f, "V12_H2"),
            RegistersType::V12_H3 => write!(f, "V12_H3"),
            RegistersType::V12_H4 => write!(f, "V12_H4"),
            RegistersType::V12_H5 => write!(f, "V12_H5"),
            RegistersType::V12_H6 => write!(f, "V12_H6"),
            RegistersType::V12_H7 => write!(f, "V12_H7"),
            RegistersType::V13_H0 => write!(f, "V13_H0"),
            RegistersType::V13_H1 => write!(f, "V13_H1"),
            RegistersType::V13_H2 => write!(f, "V13_H2"),
            RegistersType::V13_H3 => write!(f, "V13_H3"),
            RegistersType::V13_H4 => write!(f, "V13_H4"),
            RegistersType::V13_H5 => write!(f, "V13_H5"),
            RegistersType::V13_H6 => write!(f, "V13_H6"),
            RegistersType::V13_H7 => write!(f, "V13_H7"),
            RegistersType::V14_H0 => write!(f, "V14_H0"),
            RegistersType::V14_H1 => write!(f, "V14_H1"),
            RegistersType::V14_H2 => write!(f, "V14_H2"),
            RegistersType::V14_H3 => write!(f, "V14_H3"),
            RegistersType::V14_H4 => write!(f, "V14_H4"),
            RegistersType::V14_H5 => write!(f, "V14_H5"),
            RegistersType::V14_H6 => write!(f, "V14_H6"),
            RegistersType::V14_H7 => write!(f, "V14_H7"),
            RegistersType::V15_H0 => write!(f, "V15_H0"),
            RegistersType::V15_H1 => write!(f, "V15_H1"),
            RegistersType::V15_H2 => write!(f, "V15_H2"),
            RegistersType::V15_H3 => write!(f, "V15_H3"),
            RegistersType::V15_H4 => write!(f, "V15_H4"),
            RegistersType::V15_H5 => write!(f, "V15_H5"),
            RegistersType::V15_H6 => write!(f, "V15_H6"),
            RegistersType::V15_H7 => write!(f, "V15_H7"),
            RegistersType::V16_H0 => write!(f, "V16_H0"),
            RegistersType::V16_H1 => write!(f, "V16_H1"),
            RegistersType::V16_H2 => write!(f, "V16_H2"),
            RegistersType::V16_H3 => write!(f, "V16_H3"),
            RegistersType::V16_H4 => write!(f, "V16_H4"),
            RegistersType::V16_H5 => write!(f, "V16_H5"),
            RegistersType::V16_H6 => write!(f, "V16_H6"),
            RegistersType::V16_H7 => write!(f, "V16_H7"),
            RegistersType::V17_H0 => write!(f, "V17_H0"),
            RegistersType::V17_H1 => write!(f, "V17_H1"),
            RegistersType::V17_H2 => write!(f, "V17_H2"),
            RegistersType::V17_H3 => write!(f, "V17_H3"),
            RegistersType::V17_H4 => write!(f, "V17_H4"),
            RegistersType::V17_H5 => write!(f, "V17_H5"),
            RegistersType::V17_H6 => write!(f, "V17_H6"),
            RegistersType::V17_H7 => write!(f, "V17_H7"),
            RegistersType::V18_H0 => write!(f, "V18_H0"),
            RegistersType::V18_H1 => write!(f, "V18_H1"),
            RegistersType::V18_H2 => write!(f, "V18_H2"),
            RegistersType::V18_H3 => write!(f, "V18_H3"),
            RegistersType::V18_H4 => write!(f, "V18_H4"),
            RegistersType::V18_H5 => write!(f, "V18_H5"),
            RegistersType::V18_H6 => write!(f, "V18_H6"),
            RegistersType::V18_H7 => write!(f, "V18_H7"),
            RegistersType::V19_H0 => write!(f, "V19_H0"),
            RegistersType::V19_H1 => write!(f, "V19_H1"),
            RegistersType::V19_H2 => write!(f, "V19_H2"),
            RegistersType::V19_H3 => write!(f, "V19_H3"),
            RegistersType::V19_H4 => write!(f, "V19_H4"),
            RegistersType::V19_H5 => write!(f, "V19_H5"),
            RegistersType::V19_H6 => write!(f, "V19_H6"),
            RegistersType::V19_H7 => write!(f, "V19_H7"),
            RegistersType::V20_H0 => write!(f, "V20_H0"),
            RegistersType::V20_H1 => write!(f, "V20_H1"),
            RegistersType::V20_H2 => write!(f, "V20_H2"),
            RegistersType::V20_H3 => write!(f, "V20_H3"),
            RegistersType::V20_H4 => write!(f, "V20_H4"),
            RegistersType::V20_H5 => write!(f, "V20_H5"),
            RegistersType::V20_H6 => write!(f, "V20_H6"),
            RegistersType::V20_H7 => write!(f, "V20_H7"),
            RegistersType::V21_H0 => write!(f, "V21_H0"),
            RegistersType::V21_H1 => write!(f, "V21_H1"),
            RegistersType::V21_H2 => write!(f, "V21_H2"),
            RegistersType::V21_H3 => write!(f, "V21_H3"),
            RegistersType::V21_H4 => write!(f, "V21_H4"),
            RegistersType::V21_H5 => write!(f, "V21_H5"),
            RegistersType::V21_H6 => write!(f, "V21_H6"),
            RegistersType::V21_H7 => write!(f, "V21_H7"),
            RegistersType::V22_H0 => write!(f, "V22_H0"),
            RegistersType::V22_H1 => write!(f, "V22_H1"),
            RegistersType::V22_H2 => write!(f, "V22_H2"),
            RegistersType::V22_H3 => write!(f, "V22_H3"),
            RegistersType::V22_H4 => write!(f, "V22_H4"),
            RegistersType::V22_H5 => write!(f, "V22_H5"),
            RegistersType::V22_H6 => write!(f, "V22_H6"),
            RegistersType::V22_H7 => write!(f, "V22_H7"),
            RegistersType::V23_H0 => write!(f, "V23_H0"),
            RegistersType::V23_H1 => write!(f, "V23_H1"),
            RegistersType::V23_H2 => write!(f, "V23_H2"),
            RegistersType::V23_H3 => write!(f, "V23_H3"),
            RegistersType::V23_H4 => write!(f, "V23_H4"),
            RegistersType::V23_H5 => write!(f, "V23_H5"),
            RegistersType::V23_H6 => write!(f, "V23_H6"),
            RegistersType::V23_H7 => write!(f, "V23_H7"),
            RegistersType::V24_H0 => write!(f, "V24_H0"),
            RegistersType::V24_H1 => write!(f, "V24_H1"),
            RegistersType::V24_H2 => write!(f, "V24_H2"),
            RegistersType::V24_H3 => write!(f, "V24_H3"),
            RegistersType::V24_H4 => write!(f, "V24_H4"),
            RegistersType::V24_H5 => write!(f, "V24_H5"),
            RegistersType::V24_H6 => write!(f, "V24_H6"),
            RegistersType::V24_H7 => write!(f, "V24_H7"),
            RegistersType::V25_H0 => write!(f, "V25_H0"),
            RegistersType::V25_H1 => write!(f, "V25_H1"),
            RegistersType::V25_H2 => write!(f, "V25_H2"),
            RegistersType::V25_H3 => write!(f, "V25_H3"),
            RegistersType::V25_H4 => write!(f, "V25_H4"),
            RegistersType::V25_H5 => write!(f, "V25_H5"),
            RegistersType::V25_H6 => write!(f, "V25_H6"),
            RegistersType::V25_H7 => write!(f, "V25_H7"),
            RegistersType::V26_H0 => write!(f, "V26_H0"),
            RegistersType::V26_H1 => write!(f, "V26_H1"),
            RegistersType::V26_H2 => write!(f, "V26_H2"),
            RegistersType::V26_H3 => write!(f, "V26_H3"),
            RegistersType::V26_H4 => write!(f, "V26_H4"),
            RegistersType::V26_H5 => write!(f, "V26_H5"),
            RegistersType::V26_H6 => write!(f, "V26_H6"),
            RegistersType::V26_H7 => write!(f, "V26_H7"),
            RegistersType::V27_H0 => write!(f, "V27_H0"),
            RegistersType::V27_H1 => write!(f, "V27_H1"),
            RegistersType::V27_H2 => write!(f, "V27_H2"),
            RegistersType::V27_H3 => write!(f, "V27_H3"),
            RegistersType::V27_H4 => write!(f, "V27_H4"),
            RegistersType::V27_H5 => write!(f, "V27_H5"),
            RegistersType::V27_H6 => write!(f, "V27_H6"),
            RegistersType::V27_H7 => write!(f, "V27_H7"),
            RegistersType::V28_H0 => write!(f, "V28_H0"),
            RegistersType::V28_H1 => write!(f, "V28_H1"),
            RegistersType::V28_H2 => write!(f, "V28_H2"),
            RegistersType::V28_H3 => write!(f, "V28_H3"),
            RegistersType::V28_H4 => write!(f, "V28_H4"),
            RegistersType::V28_H5 => write!(f, "V28_H5"),
            RegistersType::V28_H6 => write!(f, "V28_H6"),
            RegistersType::V28_H7 => write!(f, "V28_H7"),
            RegistersType::V29_H0 => write!(f, "V29_H0"),
            RegistersType::V29_H1 => write!(f, "V29_H1"),
            RegistersType::V29_H2 => write!(f, "V29_H2"),
            RegistersType::V29_H3 => write!(f, "V29_H3"),
            RegistersType::V29_H4 => write!(f, "V29_H4"),
            RegistersType::V29_H5 => write!(f, "V29_H5"),
            RegistersType::V29_H6 => write!(f, "V29_H6"),
            RegistersType::V29_H7 => write!(f, "V29_H7"),
            RegistersType::V30_H0 => write!(f, "V30_H0"),
            RegistersType::V30_H1 => write!(f, "V30_H1"),
            RegistersType::V30_H2 => write!(f, "V30_H2"),
            RegistersType::V30_H3 => write!(f, "V30_H3"),
            RegistersType::V30_H4 => write!(f, "V30_H4"),
            RegistersType::V30_H5 => write!(f, "V30_H5"),
            RegistersType::V30_H6 => write!(f, "V30_H6"),
            RegistersType::V30_H7 => write!(f, "V30_H7"),
            RegistersType::V31_H0 => write!(f, "V31_H0"),
            RegistersType::V31_H1 => write!(f, "V31_H1"),
            RegistersType::V31_H2 => write!(f, "V31_H2"),
            RegistersType::V31_H3 => write!(f, "V31_H3"),
            RegistersType::V31_H4 => write!(f, "V31_H4"),
            RegistersType::V31_H5 => write!(f, "V31_H5"),
            RegistersType::V31_H6 => write!(f, "V31_H6"),
            RegistersType::V31_H7 => write!(f, "V31_H7"),
            RegistersType::V0_S0 => write!(f, "V0_S0"),
            RegistersType::V0_S1 => write!(f, "V0_S1"),
            RegistersType::V0_S2 => write!(f, "V0_S2"),
            RegistersType::V0_S3 => write!(f, "V0_S3"),
            RegistersType::V1_S0 => write!(f, "V1_S0"),
            RegistersType::V1_S1 => write!(f, "V1_S1"),
            RegistersType::V1_S2 => write!(f, "V1_S2"),
            RegistersType::V1_S3 => write!(f, "V1_S3"),
            RegistersType::V2_S0 => write!(f, "V2_S0"),
            RegistersType::V2_S1 => write!(f, "V2_S1"),
            RegistersType::V2_S2 => write!(f, "V2_S2"),
            RegistersType::V2_S3 => write!(f, "V2_S3"),
            RegistersType::V3_S0 => write!(f, "V3_S0"),
            RegistersType::V3_S1 => write!(f, "V3_S1"),
            RegistersType::V3_S2 => write!(f, "V3_S2"),
            RegistersType::V3_S3 => write!(f, "V3_S3"),
            RegistersType::V4_S0 => write!(f, "V4_S0"),
            RegistersType::V4_S1 => write!(f, "V4_S1"),
            RegistersType::V4_S2 => write!(f, "V4_S2"),
            RegistersType::V4_S3 => write!(f, "V4_S3"),
            RegistersType::V5_S0 => write!(f, "V5_S0"),
            RegistersType::V5_S1 => write!(f, "V5_S1"),
            RegistersType::V5_S2 => write!(f, "V5_S2"),
            RegistersType::V5_S3 => write!(f, "V5_S3"),
            RegistersType::V6_S0 => write!(f, "V6_S0"),
            RegistersType::V6_S1 => write!(f, "V6_S1"),
            RegistersType::V6_S2 => write!(f, "V6_S2"),
            RegistersType::V6_S3 => write!(f, "V6_S3"),
            RegistersType::V7_S0 => write!(f, "V7_S0"),
            RegistersType::V7_S1 => write!(f, "V7_S1"),
            RegistersType::V7_S2 => write!(f, "V7_S2"),
            RegistersType::V7_S3 => write!(f, "V7_S3"),
            RegistersType::V8_S0 => write!(f, "V8_S0"),
            RegistersType::V8_S1 => write!(f, "V8_S1"),
            RegistersType::V8_S2 => write!(f, "V8_S2"),
            RegistersType::V8_S3 => write!(f, "V8_S3"),
            RegistersType::V9_S0 => write!(f, "V9_S0"),
            RegistersType::V9_S1 => write!(f, "V9_S1"),
            RegistersType::V9_S2 => write!(f, "V9_S2"),
            RegistersType::V9_S3 => write!(f, "V9_S3"),
            RegistersType::V10_S0 => write!(f, "V10_S0"),
            RegistersType::V10_S1 => write!(f, "V10_S1"),
            RegistersType::V10_S2 => write!(f, "V10_S2"),
            RegistersType::V10_S3 => write!(f, "V10_S3"),
            RegistersType::V11_S0 => write!(f, "V11_S0"),
            RegistersType::V11_S1 => write!(f, "V11_S1"),
            RegistersType::V11_S2 => write!(f, "V11_S2"),
            RegistersType::V11_S3 => write!(f, "V11_S3"),
            RegistersType::V12_S0 => write!(f, "V12_S0"),
            RegistersType::V12_S1 => write!(f, "V12_S1"),
            RegistersType::V12_S2 => write!(f, "V12_S2"),
            RegistersType::V12_S3 => write!(f, "V12_S3"),
            RegistersType::V13_S0 => write!(f, "V13_S0"),
            RegistersType::V13_S1 => write!(f, "V13_S1"),
            RegistersType::V13_S2 => write!(f, "V13_S2"),
            RegistersType::V13_S3 => write!(f, "V13_S3"),
            RegistersType::V14_S0 => write!(f, "V14_S0"),
            RegistersType::V14_S1 => write!(f, "V14_S1"),
            RegistersType::V14_S2 => write!(f, "V14_S2"),
            RegistersType::V14_S3 => write!(f, "V14_S3"),
            RegistersType::V15_S0 => write!(f, "V15_S0"),
            RegistersType::V15_S1 => write!(f, "V15_S1"),
            RegistersType::V15_S2 => write!(f, "V15_S2"),
            RegistersType::V15_S3 => write!(f, "V15_S3"),
            RegistersType::V16_S0 => write!(f, "V16_S0"),
            RegistersType::V16_S1 => write!(f, "V16_S1"),
            RegistersType::V16_S2 => write!(f, "V16_S2"),
            RegistersType::V16_S3 => write!(f, "V16_S3"),
            RegistersType::V17_S0 => write!(f, "V17_S0"),
            RegistersType::V17_S1 => write!(f, "V17_S1"),
            RegistersType::V17_S2 => write!(f, "V17_S2"),
            RegistersType::V17_S3 => write!(f, "V17_S3"),
            RegistersType::V18_S0 => write!(f, "V18_S0"),
            RegistersType::V18_S1 => write!(f, "V18_S1"),
            RegistersType::V18_S2 => write!(f, "V18_S2"),
            RegistersType::V18_S3 => write!(f, "V18_S3"),
            RegistersType::V19_S0 => write!(f, "V19_S0"),
            RegistersType::V19_S1 => write!(f, "V19_S1"),
            RegistersType::V19_S2 => write!(f, "V19_S2"),
            RegistersType::V19_S3 => write!(f, "V19_S3"),
            RegistersType::V20_S0 => write!(f, "V20_S0"),
            RegistersType::V20_S1 => write!(f, "V20_S1"),
            RegistersType::V20_S2 => write!(f, "V20_S2"),
            RegistersType::V20_S3 => write!(f, "V20_S3"),
            RegistersType::V21_S0 => write!(f, "V21_S0"),
            RegistersType::V21_S1 => write!(f, "V21_S1"),
            RegistersType::V21_S2 => write!(f, "V21_S2"),
            RegistersType::V21_S3 => write!(f, "V21_S3"),
            RegistersType::V22_S0 => write!(f, "V22_S0"),
            RegistersType::V22_S1 => write!(f, "V22_S1"),
            RegistersType::V22_S2 => write!(f, "V22_S2"),
            RegistersType::V22_S3 => write!(f, "V22_S3"),
            RegistersType::V23_S0 => write!(f, "V23_S0"),
            RegistersType::V23_S1 => write!(f, "V23_S1"),
            RegistersType::V23_S2 => write!(f, "V23_S2"),
            RegistersType::V23_S3 => write!(f, "V23_S3"),
            RegistersType::V24_S0 => write!(f, "V24_S0"),
            RegistersType::V24_S1 => write!(f, "V24_S1"),
            RegistersType::V24_S2 => write!(f, "V24_S2"),
            RegistersType::V24_S3 => write!(f, "V24_S3"),
            RegistersType::V25_S0 => write!(f, "V25_S0"),
            RegistersType::V25_S1 => write!(f, "V25_S1"),
            RegistersType::V25_S2 => write!(f, "V25_S2"),
            RegistersType::V25_S3 => write!(f, "V25_S3"),
            RegistersType::V26_S0 => write!(f, "V26_S0"),
            RegistersType::V26_S1 => write!(f, "V26_S1"),
            RegistersType::V26_S2 => write!(f, "V26_S2"),
            RegistersType::V26_S3 => write!(f, "V26_S3"),
            RegistersType::V27_S0 => write!(f, "V27_S0"),
            RegistersType::V27_S1 => write!(f, "V27_S1"),
            RegistersType::V27_S2 => write!(f, "V27_S2"),
            RegistersType::V27_S3 => write!(f, "V27_S3"),
            RegistersType::V28_S0 => write!(f, "V28_S0"),
            RegistersType::V28_S1 => write!(f, "V28_S1"),
            RegistersType::V28_S2 => write!(f, "V28_S2"),
            RegistersType::V28_S3 => write!(f, "V28_S3"),
            RegistersType::V29_S0 => write!(f, "V29_S0"),
            RegistersType::V29_S1 => write!(f, "V29_S1"),
            RegistersType::V29_S2 => write!(f, "V29_S2"),
            RegistersType::V29_S3 => write!(f, "V29_S3"),
            RegistersType::V30_S0 => write!(f, "V30_S0"),
            RegistersType::V30_S1 => write!(f, "V30_S1"),
            RegistersType::V30_S2 => write!(f, "V30_S2"),
            RegistersType::V30_S3 => write!(f, "V30_S3"),
            RegistersType::V31_S0 => write!(f, "V31_S0"),
            RegistersType::V31_S1 => write!(f, "V31_S1"),
            RegistersType::V31_S2 => write!(f, "V31_S2"),
            RegistersType::V31_S3 => write!(f, "V31_S3"),
            RegistersType::V0_D0 => write!(f, "V0_D0"),
            RegistersType::V0_D1 => write!(f, "V0_D1"),
            RegistersType::V1_D0 => write!(f, "V1_D0"),
            RegistersType::V1_D1 => write!(f, "V1_D1"),
            RegistersType::V2_D0 => write!(f, "V2_D0"),
            RegistersType::V2_D1 => write!(f, "V2_D1"),
            RegistersType::V3_D0 => write!(f, "V3_D0"),
            RegistersType::V3_D1 => write!(f, "V3_D1"),
            RegistersType::V4_D0 => write!(f, "V4_D0"),
            RegistersType::V4_D1 => write!(f, "V4_D1"),
            RegistersType::V5_D0 => write!(f, "V5_D0"),
            RegistersType::V5_D1 => write!(f, "V5_D1"),
            RegistersType::V6_D0 => write!(f, "V6_D0"),
            RegistersType::V6_D1 => write!(f, "V6_D1"),
            RegistersType::V7_D0 => write!(f, "V7_D0"),
            RegistersType::V7_D1 => write!(f, "V7_D1"),
            RegistersType::V8_D0 => write!(f, "V8_D0"),
            RegistersType::V8_D1 => write!(f, "V8_D1"),
            RegistersType::V9_D0 => write!(f, "V9_D0"),
            RegistersType::V9_D1 => write!(f, "V9_D1"),
            RegistersType::V10_D0 => write!(f, "V10_D0"),
            RegistersType::V10_D1 => write!(f, "V10_D1"),
            RegistersType::V11_D0 => write!(f, "V11_D0"),
            RegistersType::V11_D1 => write!(f, "V11_D1"),
            RegistersType::V12_D0 => write!(f, "V12_D0"),
            RegistersType::V12_D1 => write!(f, "V12_D1"),
            RegistersType::V13_D0 => write!(f, "V13_D0"),
            RegistersType::V13_D1 => write!(f, "V13_D1"),
            RegistersType::V14_D0 => write!(f, "V14_D0"),
            RegistersType::V14_D1 => write!(f, "V14_D1"),
            RegistersType::V15_D0 => write!(f, "V15_D0"),
            RegistersType::V15_D1 => write!(f, "V15_D1"),
            RegistersType::V16_D0 => write!(f, "V16_D0"),
            RegistersType::V16_D1 => write!(f, "V16_D1"),
            RegistersType::V17_D0 => write!(f, "V17_D0"),
            RegistersType::V17_D1 => write!(f, "V17_D1"),
            RegistersType::V18_D0 => write!(f, "V18_D0"),
            RegistersType::V18_D1 => write!(f, "V18_D1"),
            RegistersType::V19_D0 => write!(f, "V19_D0"),
            RegistersType::V19_D1 => write!(f, "V19_D1"),
            RegistersType::V20_D0 => write!(f, "V20_D0"),
            RegistersType::V20_D1 => write!(f, "V20_D1"),
            RegistersType::V21_D0 => write!(f, "V21_D0"),
            RegistersType::V21_D1 => write!(f, "V21_D1"),
            RegistersType::V22_D0 => write!(f, "V22_D0"),
            RegistersType::V22_D1 => write!(f, "V22_D1"),
            RegistersType::V23_D0 => write!(f, "V23_D0"),
            RegistersType::V23_D1 => write!(f, "V23_D1"),
            RegistersType::V24_D0 => write!(f, "V24_D0"),
            RegistersType::V24_D1 => write!(f, "V24_D1"),
            RegistersType::V25_D0 => write!(f, "V25_D0"),
            RegistersType::V25_D1 => write!(f, "V25_D1"),
            RegistersType::V26_D0 => write!(f, "V26_D0"),
            RegistersType::V26_D1 => write!(f, "V26_D1"),
            RegistersType::V27_D0 => write!(f, "V27_D0"),
            RegistersType::V27_D1 => write!(f, "V27_D1"),
            RegistersType::V28_D0 => write!(f, "V28_D0"),
            RegistersType::V28_D1 => write!(f, "V28_D1"),
            RegistersType::V29_D0 => write!(f, "V29_D0"),
            RegistersType::V29_D1 => write!(f, "V29_D1"),
            RegistersType::V30_D0 => write!(f, "V30_D0"),
            RegistersType::V30_D1 => write!(f, "V30_D1"),
            RegistersType::V31_D0 => write!(f, "V31_D0"),
            RegistersType::V31_D1 => write!(f, "V31_D1"),
            RegistersType::Z0 => write!(f, "Z0"),
            RegistersType::Z1 => write!(f, "Z1"),
            RegistersType::Z2 => write!(f, "Z2"),
            RegistersType::Z3 => write!(f, "Z3"),
            RegistersType::Z4 => write!(f, "Z4"),
            RegistersType::Z5 => write!(f, "Z5"),
            RegistersType::Z6 => write!(f, "Z6"),
            RegistersType::Z7 => write!(f, "Z7"),
            RegistersType::Z8 => write!(f, "Z8"),
            RegistersType::Z9 => write!(f, "Z9"),
            RegistersType::Z10 => write!(f, "Z10"),
            RegistersType::Z11 => write!(f, "Z11"),
            RegistersType::Z12 => write!(f, "Z12"),
            RegistersType::Z13 => write!(f, "Z13"),
            RegistersType::Z14 => write!(f, "Z14"),
            RegistersType::Z15 => write!(f, "Z15"),
            RegistersType::Z16 => write!(f, "Z16"),
            RegistersType::Z17 => write!(f, "Z17"),
            RegistersType::Z18 => write!(f, "Z18"),
            RegistersType::Z19 => write!(f, "Z19"),
            RegistersType::Z20 => write!(f, "Z20"),
            RegistersType::Z21 => write!(f, "Z21"),
            RegistersType::Z22 => write!(f, "Z22"),
            RegistersType::Z23 => write!(f, "Z23"),
            RegistersType::Z24 => write!(f, "Z24"),
            RegistersType::Z25 => write!(f, "Z25"),
            RegistersType::Z26 => write!(f, "Z26"),
            RegistersType::Z27 => write!(f, "Z27"),
            RegistersType::Z28 => write!(f, "Z28"),
            RegistersType::Z29 => write!(f, "Z29"),
            RegistersType::Z30 => write!(f, "Z30"),
            RegistersType::Z31 => write!(f, "Z31"),
            RegistersType::P0 => write!(f, "P0"),
            RegistersType::P1 => write!(f, "P1"),
            RegistersType::P2 => write!(f, "P2"),
            RegistersType::P3 => write!(f, "P3"),
            RegistersType::P4 => write!(f, "P4"),
            RegistersType::P5 => write!(f, "P5"),
            RegistersType::P6 => write!(f, "P6"),
            RegistersType::P7 => write!(f, "P7"),
            RegistersType::P8 => write!(f, "P8"),
            RegistersType::P9 => write!(f, "P9"),
            RegistersType::P10 => write!(f, "P10"),
            RegistersType::P11 => write!(f, "P11"),
            RegistersType::P12 => write!(f, "P12"),
            RegistersType::P13 => write!(f, "P13"),
            RegistersType::P14 => write!(f, "P14"),
            RegistersType::P15 => write!(f, "P15"),
            RegistersType::P16 => write!(f, "P16"),
            RegistersType::P17 => write!(f, "P17"),
            RegistersType::P18 => write!(f, "P18"),
            RegistersType::P19 => write!(f, "P19"),
            RegistersType::P20 => write!(f, "P20"),
            RegistersType::P21 => write!(f, "P21"),
            RegistersType::P22 => write!(f, "P22"),
            RegistersType::P23 => write!(f, "P23"),
            RegistersType::P24 => write!(f, "P24"),
            RegistersType::P25 => write!(f, "P25"),
            RegistersType::P26 => write!(f, "P26"),
            RegistersType::P27 => write!(f, "P27"),
            RegistersType::P28 => write!(f, "P28"),
            RegistersType::P29 => write!(f, "P29"),
            RegistersType::P30 => write!(f, "P30"),
            RegistersType::P31 => write!(f, "P31"),
            RegistersType::PF0 => write!(f, "PF0"),
            RegistersType::PF1 => write!(f, "PF1"),
            RegistersType::PF2 => write!(f, "PF2"),
            RegistersType::PF3 => write!(f, "PF3"),
            RegistersType::PF4 => write!(f, "PF4"),
            RegistersType::PF5 => write!(f, "PF5"),
            RegistersType::PF6 => write!(f, "PF6"),
            RegistersType::PF7 => write!(f, "PF7"),
            RegistersType::PF8 => write!(f, "PF8"),
            RegistersType::PF9 => write!(f, "PF9"),
            RegistersType::PF10 => write!(f, "PF10"),
            RegistersType::PF11 => write!(f, "PF11"),
            RegistersType::PF12 => write!(f, "PF12"),
            RegistersType::PF13 => write!(f, "PF13"),
            RegistersType::PF14 => write!(f, "PF14"),
            RegistersType::PF15 => write!(f, "PF15"),
            RegistersType::PF16 => write!(f, "PF16"),
            RegistersType::PF17 => write!(f, "PF17"),
            RegistersType::PF18 => write!(f, "PF18"),
            RegistersType::PF19 => write!(f, "PF19"),
            RegistersType::PF20 => write!(f, "PF20"),
            RegistersType::PF21 => write!(f, "PF21"),
            RegistersType::PF22 => write!(f, "PF22"),
            RegistersType::PF23 => write!(f, "PF23"),
            RegistersType::PF24 => write!(f, "PF24"),
            RegistersType::PF25 => write!(f, "PF25"),
            RegistersType::PF26 => write!(f, "PF26"),
            RegistersType::PF27 => write!(f, "PF27"),
            RegistersType::PF28 => write!(f, "PF28"),
            RegistersType::PF29 => write!(f, "PF29"),
            RegistersType::PF30 => write!(f, "PF30"),
            RegistersType::PF31 => write!(f, "PF31"),
            RegistersType::END => write!(f, "END"),
        }
    }
}

impl From<usize> for RegistersType {
    fn from(value: usize) -> Self {
        match value {
            0x0 => Self::NONE,
            0x1 => Self::W0,
            0x2 => Self::W1,
            0x3 => Self::W2,
            0x4 => Self::W3,
            0x5 => Self::W4,
            0x6 => Self::W5,
            0x7 => Self::W6,
            0x8 => Self::W7,
            0x9 => Self::W8,
            0xa => Self::W9,
            0xb => Self::W10,
            0xc => Self::W11,
            0xd => Self::W12,
            0xe => Self::W13,
            0xf => Self::W14,
            0x10 => Self::W15,
            0x11 => Self::W16,
            0x12 => Self::W17,
            0x13 => Self::W18,
            0x14 => Self::W19,
            0x15 => Self::W20,
            0x16 => Self::W21,
            0x17 => Self::W22,
            0x18 => Self::W23,
            0x19 => Self::W24,
            0x1a => Self::W25,
            0x1b => Self::W26,
            0x1c => Self::W27,
            0x1d => Self::W28,
            0x1e => Self::W29,
            0x1f => Self::W30,
            0x20 => Self::WZR,
            0x21 => Self::WSP,
            0x22 => Self::X0,
            0x23 => Self::X1,
            0x24 => Self::X2,
            0x25 => Self::X3,
            0x26 => Self::X4,
            0x27 => Self::X5,
            0x28 => Self::X6,
            0x29 => Self::X7,
            0x2a => Self::X8,
            0x2b => Self::X9,
            0x2c => Self::X10,
            0x2d => Self::X11,
            0x2e => Self::X12,
            0x2f => Self::X13,
            0x30 => Self::X14,
            0x31 => Self::X15,
            0x32 => Self::X16,
            0x33 => Self::X17,
            0x34 => Self::X18,
            0x35 => Self::X19,
            0x36 => Self::X20,
            0x37 => Self::X21,
            0x38 => Self::X22,
            0x39 => Self::X23,
            0x3a => Self::X24,
            0x3b => Self::X25,
            0x3c => Self::X26,
            0x3d => Self::X27,
            0x3e => Self::X28,
            0x3f => Self::X29,
            0x40 => Self::X30,
            0x41 => Self::XZR,
            0x42 => Self::SP,
            0x43 => Self::V0,
            0x44 => Self::V1,
            0x45 => Self::V2,
            0x46 => Self::V3,
            0x47 => Self::V4,
            0x48 => Self::V5,
            0x49 => Self::V6,
            0x4a => Self::V7,
            0x4b => Self::V8,
            0x4c => Self::V9,
            0x4d => Self::V10,
            0x4e => Self::V11,
            0x4f => Self::V12,
            0x50 => Self::V13,
            0x51 => Self::V14,
            0x52 => Self::V15,
            0x53 => Self::V16,
            0x54 => Self::V17,
            0x55 => Self::V18,
            0x56 => Self::V19,
            0x57 => Self::V20,
            0x58 => Self::V21,
            0x59 => Self::V22,
            0x5a => Self::V23,
            0x5b => Self::V24,
            0x5c => Self::V25,
            0x5d => Self::V26,
            0x5e => Self::V27,
            0x5f => Self::V28,
            0x60 => Self::V29,
            0x61 => Self::V30,
            0x62 => Self::V31,
            0x63 => Self::B0,
            0x64 => Self::B1,
            0x65 => Self::B2,
            0x66 => Self::B3,
            0x67 => Self::B4,
            0x68 => Self::B5,
            0x69 => Self::B6,
            0x6a => Self::B7,
            0x6b => Self::B8,
            0x6c => Self::B9,
            0x6d => Self::B10,
            0x6e => Self::B11,
            0x6f => Self::B12,
            0x70 => Self::B13,
            0x71 => Self::B14,
            0x72 => Self::B15,
            0x73 => Self::B16,
            0x74 => Self::B17,
            0x75 => Self::B18,
            0x76 => Self::B19,
            0x77 => Self::B20,
            0x78 => Self::B21,
            0x79 => Self::B22,
            0x7a => Self::B23,
            0x7b => Self::B24,
            0x7c => Self::B25,
            0x7d => Self::B26,
            0x7e => Self::B27,
            0x7f => Self::B28,
            0x80 => Self::B29,
            0x81 => Self::B30,
            0x82 => Self::B31,
            0x83 => Self::H0,
            0x84 => Self::H1,
            0x85 => Self::H2,
            0x86 => Self::H3,
            0x87 => Self::H4,
            0x88 => Self::H5,
            0x89 => Self::H6,
            0x8a => Self::H7,
            0x8b => Self::H8,
            0x8c => Self::H9,
            0x8d => Self::H10,
            0x8e => Self::H11,
            0x8f => Self::H12,
            0x90 => Self::H13,
            0x91 => Self::H14,
            0x92 => Self::H15,
            0x93 => Self::H16,
            0x94 => Self::H17,
            0x95 => Self::H18,
            0x96 => Self::H19,
            0x97 => Self::H20,
            0x98 => Self::H21,
            0x99 => Self::H22,
            0x9a => Self::H23,
            0x9b => Self::H24,
            0x9c => Self::H25,
            0x9d => Self::H26,
            0x9e => Self::H27,
            0x9f => Self::H28,
            0xa0 => Self::H29,
            0xa1 => Self::H30,
            0xa2 => Self::H31,
            0xa3 => Self::S0,
            0xa4 => Self::S1,
            0xa5 => Self::S2,
            0xa6 => Self::S3,
            0xa7 => Self::S4,
            0xa8 => Self::S5,
            0xa9 => Self::S6,
            0xaa => Self::S7,
            0xab => Self::S8,
            0xac => Self::S9,
            0xad => Self::S10,
            0xae => Self::S11,
            0xaf => Self::S12,
            0xb0 => Self::S13,
            0xb1 => Self::S14,
            0xb2 => Self::S15,
            0xb3 => Self::S16,
            0xb4 => Self::S17,
            0xb5 => Self::S18,
            0xb6 => Self::S19,
            0xb7 => Self::S20,
            0xb8 => Self::S21,
            0xb9 => Self::S22,
            0xba => Self::S23,
            0xbb => Self::S24,
            0xbc => Self::S25,
            0xbd => Self::S26,
            0xbe => Self::S27,
            0xbf => Self::S28,
            0xc0 => Self::S29,
            0xc1 => Self::S30,
            0xc2 => Self::S31,
            0xc3 => Self::D0,
            0xc4 => Self::D1,
            0xc5 => Self::D2,
            0xc6 => Self::D3,
            0xc7 => Self::D4,
            0xc8 => Self::D5,
            0xc9 => Self::D6,
            0xca => Self::D7,
            0xcb => Self::D8,
            0xcc => Self::D9,
            0xcd => Self::D10,
            0xce => Self::D11,
            0xcf => Self::D12,
            0xd0 => Self::D13,
            0xd1 => Self::D14,
            0xd2 => Self::D15,
            0xd3 => Self::D16,
            0xd4 => Self::D17,
            0xd5 => Self::D18,
            0xd6 => Self::D19,
            0xd7 => Self::D20,
            0xd8 => Self::D21,
            0xd9 => Self::D22,
            0xda => Self::D23,
            0xdb => Self::D24,
            0xdc => Self::D25,
            0xdd => Self::D26,
            0xde => Self::D27,
            0xdf => Self::D28,
            0xe0 => Self::D29,
            0xe1 => Self::D30,
            0xe2 => Self::D31,
            0xe3 => Self::Q0,
            0xe4 => Self::Q1,
            0xe5 => Self::Q2,
            0xe6 => Self::Q3,
            0xe7 => Self::Q4,
            0xe8 => Self::Q5,
            0xe9 => Self::Q6,
            0xea => Self::Q7,
            0xeb => Self::Q8,
            0xec => Self::Q9,
            0xed => Self::Q10,
            0xee => Self::Q11,
            0xef => Self::Q12,
            0xf0 => Self::Q13,
            0xf1 => Self::Q14,
            0xf2 => Self::Q15,
            0xf3 => Self::Q16,
            0xf4 => Self::Q17,
            0xf5 => Self::Q18,
            0xf6 => Self::Q19,
            0xf7 => Self::Q20,
            0xf8 => Self::Q21,
            0xf9 => Self::Q22,
            0xfa => Self::Q23,
            0xfb => Self::Q24,
            0xfc => Self::Q25,
            0xfd => Self::Q26,
            0xfe => Self::Q27,
            0xff => Self::Q28,
            0x100 => Self::Q29,
            0x101 => Self::Q30,
            0x102 => Self::Q31,
            0x103 => Self::V0_B0,
            0x104 => Self::V0_B1,
            0x105 => Self::V0_B2,
            0x106 => Self::V0_B3,
            0x107 => Self::V0_B4,
            0x108 => Self::V0_B5,
            0x109 => Self::V0_B6,
            0x10a => Self::V0_B7,
            0x10b => Self::V0_B8,
            0x10c => Self::V0_B9,
            0x10d => Self::V0_B10,
            0x10e => Self::V0_B11,
            0x10f => Self::V0_B12,
            0x110 => Self::V0_B13,
            0x111 => Self::V0_B14,
            0x112 => Self::V0_B15,
            0x113 => Self::V1_B0,
            0x114 => Self::V1_B1,
            0x115 => Self::V1_B2,
            0x116 => Self::V1_B3,
            0x117 => Self::V1_B4,
            0x118 => Self::V1_B5,
            0x119 => Self::V1_B6,
            0x11a => Self::V1_B7,
            0x11b => Self::V1_B8,
            0x11c => Self::V1_B9,
            0x11d => Self::V1_B10,
            0x11e => Self::V1_B11,
            0x11f => Self::V1_B12,
            0x120 => Self::V1_B13,
            0x121 => Self::V1_B14,
            0x122 => Self::V1_B15,
            0x123 => Self::V2_B0,
            0x124 => Self::V2_B1,
            0x125 => Self::V2_B2,
            0x126 => Self::V2_B3,
            0x127 => Self::V2_B4,
            0x128 => Self::V2_B5,
            0x129 => Self::V2_B6,
            0x12a => Self::V2_B7,
            0x12b => Self::V2_B8,
            0x12c => Self::V2_B9,
            0x12d => Self::V2_B10,
            0x12e => Self::V2_B11,
            0x12f => Self::V2_B12,
            0x130 => Self::V2_B13,
            0x131 => Self::V2_B14,
            0x132 => Self::V2_B15,
            0x133 => Self::V3_B0,
            0x134 => Self::V3_B1,
            0x135 => Self::V3_B2,
            0x136 => Self::V3_B3,
            0x137 => Self::V3_B4,
            0x138 => Self::V3_B5,
            0x139 => Self::V3_B6,
            0x13a => Self::V3_B7,
            0x13b => Self::V3_B8,
            0x13c => Self::V3_B9,
            0x13d => Self::V3_B10,
            0x13e => Self::V3_B11,
            0x13f => Self::V3_B12,
            0x140 => Self::V3_B13,
            0x141 => Self::V3_B14,
            0x142 => Self::V3_B15,
            0x143 => Self::V4_B0,
            0x144 => Self::V4_B1,
            0x145 => Self::V4_B2,
            0x146 => Self::V4_B3,
            0x147 => Self::V4_B4,
            0x148 => Self::V4_B5,
            0x149 => Self::V4_B6,
            0x14a => Self::V4_B7,
            0x14b => Self::V4_B8,
            0x14c => Self::V4_B9,
            0x14d => Self::V4_B10,
            0x14e => Self::V4_B11,
            0x14f => Self::V4_B12,
            0x150 => Self::V4_B13,
            0x151 => Self::V4_B14,
            0x152 => Self::V4_B15,
            0x153 => Self::V5_B0,
            0x154 => Self::V5_B1,
            0x155 => Self::V5_B2,
            0x156 => Self::V5_B3,
            0x157 => Self::V5_B4,
            0x158 => Self::V5_B5,
            0x159 => Self::V5_B6,
            0x15a => Self::V5_B7,
            0x15b => Self::V5_B8,
            0x15c => Self::V5_B9,
            0x15d => Self::V5_B10,
            0x15e => Self::V5_B11,
            0x15f => Self::V5_B12,
            0x160 => Self::V5_B13,
            0x161 => Self::V5_B14,
            0x162 => Self::V5_B15,
            0x163 => Self::V6_B0,
            0x164 => Self::V6_B1,
            0x165 => Self::V6_B2,
            0x166 => Self::V6_B3,
            0x167 => Self::V6_B4,
            0x168 => Self::V6_B5,
            0x169 => Self::V6_B6,
            0x16a => Self::V6_B7,
            0x16b => Self::V6_B8,
            0x16c => Self::V6_B9,
            0x16d => Self::V6_B10,
            0x16e => Self::V6_B11,
            0x16f => Self::V6_B12,
            0x170 => Self::V6_B13,
            0x171 => Self::V6_B14,
            0x172 => Self::V6_B15,
            0x173 => Self::V7_B0,
            0x174 => Self::V7_B1,
            0x175 => Self::V7_B2,
            0x176 => Self::V7_B3,
            0x177 => Self::V7_B4,
            0x178 => Self::V7_B5,
            0x179 => Self::V7_B6,
            0x17a => Self::V7_B7,
            0x17b => Self::V7_B8,
            0x17c => Self::V7_B9,
            0x17d => Self::V7_B10,
            0x17e => Self::V7_B11,
            0x17f => Self::V7_B12,
            0x180 => Self::V7_B13,
            0x181 => Self::V7_B14,
            0x182 => Self::V7_B15,
            0x183 => Self::V8_B0,
            0x184 => Self::V8_B1,
            0x185 => Self::V8_B2,
            0x186 => Self::V8_B3,
            0x187 => Self::V8_B4,
            0x188 => Self::V8_B5,
            0x189 => Self::V8_B6,
            0x18a => Self::V8_B7,
            0x18b => Self::V8_B8,
            0x18c => Self::V8_B9,
            0x18d => Self::V8_B10,
            0x18e => Self::V8_B11,
            0x18f => Self::V8_B12,
            0x190 => Self::V8_B13,
            0x191 => Self::V8_B14,
            0x192 => Self::V8_B15,
            0x193 => Self::V9_B0,
            0x194 => Self::V9_B1,
            0x195 => Self::V9_B2,
            0x196 => Self::V9_B3,
            0x197 => Self::V9_B4,
            0x198 => Self::V9_B5,
            0x199 => Self::V9_B6,
            0x19a => Self::V9_B7,
            0x19b => Self::V9_B8,
            0x19c => Self::V9_B9,
            0x19d => Self::V9_B10,
            0x19e => Self::V9_B11,
            0x19f => Self::V9_B12,
            0x1a0 => Self::V9_B13,
            0x1a1 => Self::V9_B14,
            0x1a2 => Self::V9_B15,
            0x1a3 => Self::V10_B0,
            0x1a4 => Self::V10_B1,
            0x1a5 => Self::V10_B2,
            0x1a6 => Self::V10_B3,
            0x1a7 => Self::V10_B4,
            0x1a8 => Self::V10_B5,
            0x1a9 => Self::V10_B6,
            0x1aa => Self::V10_B7,
            0x1ab => Self::V10_B8,
            0x1ac => Self::V10_B9,
            0x1ad => Self::V10_B10,
            0x1ae => Self::V10_B11,
            0x1af => Self::V10_B12,
            0x1b0 => Self::V10_B13,
            0x1b1 => Self::V10_B14,
            0x1b2 => Self::V10_B15,
            0x1b3 => Self::V11_B0,
            0x1b4 => Self::V11_B1,
            0x1b5 => Self::V11_B2,
            0x1b6 => Self::V11_B3,
            0x1b7 => Self::V11_B4,
            0x1b8 => Self::V11_B5,
            0x1b9 => Self::V11_B6,
            0x1ba => Self::V11_B7,
            0x1bb => Self::V11_B8,
            0x1bc => Self::V11_B9,
            0x1bd => Self::V11_B10,
            0x1be => Self::V11_B11,
            0x1bf => Self::V11_B12,
            0x1c0 => Self::V11_B13,
            0x1c1 => Self::V11_B14,
            0x1c2 => Self::V11_B15,
            0x1c3 => Self::V12_B0,
            0x1c4 => Self::V12_B1,
            0x1c5 => Self::V12_B2,
            0x1c6 => Self::V12_B3,
            0x1c7 => Self::V12_B4,
            0x1c8 => Self::V12_B5,
            0x1c9 => Self::V12_B6,
            0x1ca => Self::V12_B7,
            0x1cb => Self::V12_B8,
            0x1cc => Self::V12_B9,
            0x1cd => Self::V12_B10,
            0x1ce => Self::V12_B11,
            0x1cf => Self::V12_B12,
            0x1d0 => Self::V12_B13,
            0x1d1 => Self::V12_B14,
            0x1d2 => Self::V12_B15,
            0x1d3 => Self::V13_B0,
            0x1d4 => Self::V13_B1,
            0x1d5 => Self::V13_B2,
            0x1d6 => Self::V13_B3,
            0x1d7 => Self::V13_B4,
            0x1d8 => Self::V13_B5,
            0x1d9 => Self::V13_B6,
            0x1da => Self::V13_B7,
            0x1db => Self::V13_B8,
            0x1dc => Self::V13_B9,
            0x1dd => Self::V13_B10,
            0x1de => Self::V13_B11,
            0x1df => Self::V13_B12,
            0x1e0 => Self::V13_B13,
            0x1e1 => Self::V13_B14,
            0x1e2 => Self::V13_B15,
            0x1e3 => Self::V14_B0,
            0x1e4 => Self::V14_B1,
            0x1e5 => Self::V14_B2,
            0x1e6 => Self::V14_B3,
            0x1e7 => Self::V14_B4,
            0x1e8 => Self::V14_B5,
            0x1e9 => Self::V14_B6,
            0x1ea => Self::V14_B7,
            0x1eb => Self::V14_B8,
            0x1ec => Self::V14_B9,
            0x1ed => Self::V14_B10,
            0x1ee => Self::V14_B11,
            0x1ef => Self::V14_B12,
            0x1f0 => Self::V14_B13,
            0x1f1 => Self::V14_B14,
            0x1f2 => Self::V14_B15,
            0x1f3 => Self::V15_B0,
            0x1f4 => Self::V15_B1,
            0x1f5 => Self::V15_B2,
            0x1f6 => Self::V15_B3,
            0x1f7 => Self::V15_B4,
            0x1f8 => Self::V15_B5,
            0x1f9 => Self::V15_B6,
            0x1fa => Self::V15_B7,
            0x1fb => Self::V15_B8,
            0x1fc => Self::V15_B9,
            0x1fd => Self::V15_B10,
            0x1fe => Self::V15_B11,
            0x1ff => Self::V15_B12,
            0x200 => Self::V15_B13,
            0x201 => Self::V15_B14,
            0x202 => Self::V15_B15,
            0x203 => Self::V16_B0,
            0x204 => Self::V16_B1,
            0x205 => Self::V16_B2,
            0x206 => Self::V16_B3,
            0x207 => Self::V16_B4,
            0x208 => Self::V16_B5,
            0x209 => Self::V16_B6,
            0x20a => Self::V16_B7,
            0x20b => Self::V16_B8,
            0x20c => Self::V16_B9,
            0x20d => Self::V16_B10,
            0x20e => Self::V16_B11,
            0x20f => Self::V16_B12,
            0x210 => Self::V16_B13,
            0x211 => Self::V16_B14,
            0x212 => Self::V16_B15,
            0x213 => Self::V17_B0,
            0x214 => Self::V17_B1,
            0x215 => Self::V17_B2,
            0x216 => Self::V17_B3,
            0x217 => Self::V17_B4,
            0x218 => Self::V17_B5,
            0x219 => Self::V17_B6,
            0x21a => Self::V17_B7,
            0x21b => Self::V17_B8,
            0x21c => Self::V17_B9,
            0x21d => Self::V17_B10,
            0x21e => Self::V17_B11,
            0x21f => Self::V17_B12,
            0x220 => Self::V17_B13,
            0x221 => Self::V17_B14,
            0x222 => Self::V17_B15,
            0x223 => Self::V18_B0,
            0x224 => Self::V18_B1,
            0x225 => Self::V18_B2,
            0x226 => Self::V18_B3,
            0x227 => Self::V18_B4,
            0x228 => Self::V18_B5,
            0x229 => Self::V18_B6,
            0x22a => Self::V18_B7,
            0x22b => Self::V18_B8,
            0x22c => Self::V18_B9,
            0x22d => Self::V18_B10,
            0x22e => Self::V18_B11,
            0x22f => Self::V18_B12,
            0x230 => Self::V18_B13,
            0x231 => Self::V18_B14,
            0x232 => Self::V18_B15,
            0x233 => Self::V19_B0,
            0x234 => Self::V19_B1,
            0x235 => Self::V19_B2,
            0x236 => Self::V19_B3,
            0x237 => Self::V19_B4,
            0x238 => Self::V19_B5,
            0x239 => Self::V19_B6,
            0x23a => Self::V19_B7,
            0x23b => Self::V19_B8,
            0x23c => Self::V19_B9,
            0x23d => Self::V19_B10,
            0x23e => Self::V19_B11,
            0x23f => Self::V19_B12,
            0x240 => Self::V19_B13,
            0x241 => Self::V19_B14,
            0x242 => Self::V19_B15,
            0x243 => Self::V20_B0,
            0x244 => Self::V20_B1,
            0x245 => Self::V20_B2,
            0x246 => Self::V20_B3,
            0x247 => Self::V20_B4,
            0x248 => Self::V20_B5,
            0x249 => Self::V20_B6,
            0x24a => Self::V20_B7,
            0x24b => Self::V20_B8,
            0x24c => Self::V20_B9,
            0x24d => Self::V20_B10,
            0x24e => Self::V20_B11,
            0x24f => Self::V20_B12,
            0x250 => Self::V20_B13,
            0x251 => Self::V20_B14,
            0x252 => Self::V20_B15,
            0x253 => Self::V21_B0,
            0x254 => Self::V21_B1,
            0x255 => Self::V21_B2,
            0x256 => Self::V21_B3,
            0x257 => Self::V21_B4,
            0x258 => Self::V21_B5,
            0x259 => Self::V21_B6,
            0x25a => Self::V21_B7,
            0x25b => Self::V21_B8,
            0x25c => Self::V21_B9,
            0x25d => Self::V21_B10,
            0x25e => Self::V21_B11,
            0x25f => Self::V21_B12,
            0x260 => Self::V21_B13,
            0x261 => Self::V21_B14,
            0x262 => Self::V21_B15,
            0x263 => Self::V22_B0,
            0x264 => Self::V22_B1,
            0x265 => Self::V22_B2,
            0x266 => Self::V22_B3,
            0x267 => Self::V22_B4,
            0x268 => Self::V22_B5,
            0x269 => Self::V22_B6,
            0x26a => Self::V22_B7,
            0x26b => Self::V22_B8,
            0x26c => Self::V22_B9,
            0x26d => Self::V22_B10,
            0x26e => Self::V22_B11,
            0x26f => Self::V22_B12,
            0x270 => Self::V22_B13,
            0x271 => Self::V22_B14,
            0x272 => Self::V22_B15,
            0x273 => Self::V23_B0,
            0x274 => Self::V23_B1,
            0x275 => Self::V23_B2,
            0x276 => Self::V23_B3,
            0x277 => Self::V23_B4,
            0x278 => Self::V23_B5,
            0x279 => Self::V23_B6,
            0x27a => Self::V23_B7,
            0x27b => Self::V23_B8,
            0x27c => Self::V23_B9,
            0x27d => Self::V23_B10,
            0x27e => Self::V23_B11,
            0x27f => Self::V23_B12,
            0x280 => Self::V23_B13,
            0x281 => Self::V23_B14,
            0x282 => Self::V23_B15,
            0x283 => Self::V24_B0,
            0x284 => Self::V24_B1,
            0x285 => Self::V24_B2,
            0x286 => Self::V24_B3,
            0x287 => Self::V24_B4,
            0x288 => Self::V24_B5,
            0x289 => Self::V24_B6,
            0x28a => Self::V24_B7,
            0x28b => Self::V24_B8,
            0x28c => Self::V24_B9,
            0x28d => Self::V24_B10,
            0x28e => Self::V24_B11,
            0x28f => Self::V24_B12,
            0x290 => Self::V24_B13,
            0x291 => Self::V24_B14,
            0x292 => Self::V24_B15,
            0x293 => Self::V25_B0,
            0x294 => Self::V25_B1,
            0x295 => Self::V25_B2,
            0x296 => Self::V25_B3,
            0x297 => Self::V25_B4,
            0x298 => Self::V25_B5,
            0x299 => Self::V25_B6,
            0x29a => Self::V25_B7,
            0x29b => Self::V25_B8,
            0x29c => Self::V25_B9,
            0x29d => Self::V25_B10,
            0x29e => Self::V25_B11,
            0x29f => Self::V25_B12,
            0x2a0 => Self::V25_B13,
            0x2a1 => Self::V25_B14,
            0x2a2 => Self::V25_B15,
            0x2a3 => Self::V26_B0,
            0x2a4 => Self::V26_B1,
            0x2a5 => Self::V26_B2,
            0x2a6 => Self::V26_B3,
            0x2a7 => Self::V26_B4,
            0x2a8 => Self::V26_B5,
            0x2a9 => Self::V26_B6,
            0x2aa => Self::V26_B7,
            0x2ab => Self::V26_B8,
            0x2ac => Self::V26_B9,
            0x2ad => Self::V26_B10,
            0x2ae => Self::V26_B11,
            0x2af => Self::V26_B12,
            0x2b0 => Self::V26_B13,
            0x2b1 => Self::V26_B14,
            0x2b2 => Self::V26_B15,
            0x2b3 => Self::V27_B0,
            0x2b4 => Self::V27_B1,
            0x2b5 => Self::V27_B2,
            0x2b6 => Self::V27_B3,
            0x2b7 => Self::V27_B4,
            0x2b8 => Self::V27_B5,
            0x2b9 => Self::V27_B6,
            0x2ba => Self::V27_B7,
            0x2bb => Self::V27_B8,
            0x2bc => Self::V27_B9,
            0x2bd => Self::V27_B10,
            0x2be => Self::V27_B11,
            0x2bf => Self::V27_B12,
            0x2c0 => Self::V27_B13,
            0x2c1 => Self::V27_B14,
            0x2c2 => Self::V27_B15,
            0x2c3 => Self::V28_B0,
            0x2c4 => Self::V28_B1,
            0x2c5 => Self::V28_B2,
            0x2c6 => Self::V28_B3,
            0x2c7 => Self::V28_B4,
            0x2c8 => Self::V28_B5,
            0x2c9 => Self::V28_B6,
            0x2ca => Self::V28_B7,
            0x2cb => Self::V28_B8,
            0x2cc => Self::V28_B9,
            0x2cd => Self::V28_B10,
            0x2ce => Self::V28_B11,
            0x2cf => Self::V28_B12,
            0x2d0 => Self::V28_B13,
            0x2d1 => Self::V28_B14,
            0x2d2 => Self::V28_B15,
            0x2d3 => Self::V29_B0,
            0x2d4 => Self::V29_B1,
            0x2d5 => Self::V29_B2,
            0x2d6 => Self::V29_B3,
            0x2d7 => Self::V29_B4,
            0x2d8 => Self::V29_B5,
            0x2d9 => Self::V29_B6,
            0x2da => Self::V29_B7,
            0x2db => Self::V29_B8,
            0x2dc => Self::V29_B9,
            0x2dd => Self::V29_B10,
            0x2de => Self::V29_B11,
            0x2df => Self::V29_B12,
            0x2e0 => Self::V29_B13,
            0x2e1 => Self::V29_B14,
            0x2e2 => Self::V29_B15,
            0x2e3 => Self::V30_B0,
            0x2e4 => Self::V30_B1,
            0x2e5 => Self::V30_B2,
            0x2e6 => Self::V30_B3,
            0x2e7 => Self::V30_B4,
            0x2e8 => Self::V30_B5,
            0x2e9 => Self::V30_B6,
            0x2ea => Self::V30_B7,
            0x2eb => Self::V30_B8,
            0x2ec => Self::V30_B9,
            0x2ed => Self::V30_B10,
            0x2ee => Self::V30_B11,
            0x2ef => Self::V30_B12,
            0x2f0 => Self::V30_B13,
            0x2f1 => Self::V30_B14,
            0x2f2 => Self::V30_B15,
            0x2f3 => Self::V31_B0,
            0x2f4 => Self::V31_B1,
            0x2f5 => Self::V31_B2,
            0x2f6 => Self::V31_B3,
            0x2f7 => Self::V31_B4,
            0x2f8 => Self::V31_B5,
            0x2f9 => Self::V31_B6,
            0x2fa => Self::V31_B7,
            0x2fb => Self::V31_B8,
            0x2fc => Self::V31_B9,
            0x2fd => Self::V31_B10,
            0x2fe => Self::V31_B11,
            0x2ff => Self::V31_B12,
            0x300 => Self::V31_B13,
            0x301 => Self::V31_B14,
            0x302 => Self::V31_B15,
            0x303 => Self::V0_H0,
            0x304 => Self::V0_H1,
            0x305 => Self::V0_H2,
            0x306 => Self::V0_H3,
            0x307 => Self::V0_H4,
            0x308 => Self::V0_H5,
            0x309 => Self::V0_H6,
            0x30a => Self::V0_H7,
            0x30b => Self::V1_H0,
            0x30c => Self::V1_H1,
            0x30d => Self::V1_H2,
            0x30e => Self::V1_H3,
            0x30f => Self::V1_H4,
            0x310 => Self::V1_H5,
            0x311 => Self::V1_H6,
            0x312 => Self::V1_H7,
            0x313 => Self::V2_H0,
            0x314 => Self::V2_H1,
            0x315 => Self::V2_H2,
            0x316 => Self::V2_H3,
            0x317 => Self::V2_H4,
            0x318 => Self::V2_H5,
            0x319 => Self::V2_H6,
            0x31a => Self::V2_H7,
            0x31b => Self::V3_H0,
            0x31c => Self::V3_H1,
            0x31d => Self::V3_H2,
            0x31e => Self::V3_H3,
            0x31f => Self::V3_H4,
            0x320 => Self::V3_H5,
            0x321 => Self::V3_H6,
            0x322 => Self::V3_H7,
            0x323 => Self::V4_H0,
            0x324 => Self::V4_H1,
            0x325 => Self::V4_H2,
            0x326 => Self::V4_H3,
            0x327 => Self::V4_H4,
            0x328 => Self::V4_H5,
            0x329 => Self::V4_H6,
            0x32a => Self::V4_H7,
            0x32b => Self::V5_H0,
            0x32c => Self::V5_H1,
            0x32d => Self::V5_H2,
            0x32e => Self::V5_H3,
            0x32f => Self::V5_H4,
            0x330 => Self::V5_H5,
            0x331 => Self::V5_H6,
            0x332 => Self::V5_H7,
            0x333 => Self::V6_H0,
            0x334 => Self::V6_H1,
            0x335 => Self::V6_H2,
            0x336 => Self::V6_H3,
            0x337 => Self::V6_H4,
            0x338 => Self::V6_H5,
            0x339 => Self::V6_H6,
            0x33a => Self::V6_H7,
            0x33b => Self::V7_H0,
            0x33c => Self::V7_H1,
            0x33d => Self::V7_H2,
            0x33e => Self::V7_H3,
            0x33f => Self::V7_H4,
            0x340 => Self::V7_H5,
            0x341 => Self::V7_H6,
            0x342 => Self::V7_H7,
            0x343 => Self::V8_H0,
            0x344 => Self::V8_H1,
            0x345 => Self::V8_H2,
            0x346 => Self::V8_H3,
            0x347 => Self::V8_H4,
            0x348 => Self::V8_H5,
            0x349 => Self::V8_H6,
            0x34a => Self::V8_H7,
            0x34b => Self::V9_H0,
            0x34c => Self::V9_H1,
            0x34d => Self::V9_H2,
            0x34e => Self::V9_H3,
            0x34f => Self::V9_H4,
            0x350 => Self::V9_H5,
            0x351 => Self::V9_H6,
            0x352 => Self::V9_H7,
            0x353 => Self::V10_H0,
            0x354 => Self::V10_H1,
            0x355 => Self::V10_H2,
            0x356 => Self::V10_H3,
            0x357 => Self::V10_H4,
            0x358 => Self::V10_H5,
            0x359 => Self::V10_H6,
            0x35a => Self::V10_H7,
            0x35b => Self::V11_H0,
            0x35c => Self::V11_H1,
            0x35d => Self::V11_H2,
            0x35e => Self::V11_H3,
            0x35f => Self::V11_H4,
            0x360 => Self::V11_H5,
            0x361 => Self::V11_H6,
            0x362 => Self::V11_H7,
            0x363 => Self::V12_H0,
            0x364 => Self::V12_H1,
            0x365 => Self::V12_H2,
            0x366 => Self::V12_H3,
            0x367 => Self::V12_H4,
            0x368 => Self::V12_H5,
            0x369 => Self::V12_H6,
            0x36a => Self::V12_H7,
            0x36b => Self::V13_H0,
            0x36c => Self::V13_H1,
            0x36d => Self::V13_H2,
            0x36e => Self::V13_H3,
            0x36f => Self::V13_H4,
            0x370 => Self::V13_H5,
            0x371 => Self::V13_H6,
            0x372 => Self::V13_H7,
            0x373 => Self::V14_H0,
            0x374 => Self::V14_H1,
            0x375 => Self::V14_H2,
            0x376 => Self::V14_H3,
            0x377 => Self::V14_H4,
            0x378 => Self::V14_H5,
            0x379 => Self::V14_H6,
            0x37a => Self::V14_H7,
            0x37b => Self::V15_H0,
            0x37c => Self::V15_H1,
            0x37d => Self::V15_H2,
            0x37e => Self::V15_H3,
            0x37f => Self::V15_H4,
            0x380 => Self::V15_H5,
            0x381 => Self::V15_H6,
            0x382 => Self::V15_H7,
            0x383 => Self::V16_H0,
            0x384 => Self::V16_H1,
            0x385 => Self::V16_H2,
            0x386 => Self::V16_H3,
            0x387 => Self::V16_H4,
            0x388 => Self::V16_H5,
            0x389 => Self::V16_H6,
            0x38a => Self::V16_H7,
            0x38b => Self::V17_H0,
            0x38c => Self::V17_H1,
            0x38d => Self::V17_H2,
            0x38e => Self::V17_H3,
            0x38f => Self::V17_H4,
            0x390 => Self::V17_H5,
            0x391 => Self::V17_H6,
            0x392 => Self::V17_H7,
            0x393 => Self::V18_H0,
            0x394 => Self::V18_H1,
            0x395 => Self::V18_H2,
            0x396 => Self::V18_H3,
            0x397 => Self::V18_H4,
            0x398 => Self::V18_H5,
            0x399 => Self::V18_H6,
            0x39a => Self::V18_H7,
            0x39b => Self::V19_H0,
            0x39c => Self::V19_H1,
            0x39d => Self::V19_H2,
            0x39e => Self::V19_H3,
            0x39f => Self::V19_H4,
            0x3a0 => Self::V19_H5,
            0x3a1 => Self::V19_H6,
            0x3a2 => Self::V19_H7,
            0x3a3 => Self::V20_H0,
            0x3a4 => Self::V20_H1,
            0x3a5 => Self::V20_H2,
            0x3a6 => Self::V20_H3,
            0x3a7 => Self::V20_H4,
            0x3a8 => Self::V20_H5,
            0x3a9 => Self::V20_H6,
            0x3aa => Self::V20_H7,
            0x3ab => Self::V21_H0,
            0x3ac => Self::V21_H1,
            0x3ad => Self::V21_H2,
            0x3ae => Self::V21_H3,
            0x3af => Self::V21_H4,
            0x3b0 => Self::V21_H5,
            0x3b1 => Self::V21_H6,
            0x3b2 => Self::V21_H7,
            0x3b3 => Self::V22_H0,
            0x3b4 => Self::V22_H1,
            0x3b5 => Self::V22_H2,
            0x3b6 => Self::V22_H3,
            0x3b7 => Self::V22_H4,
            0x3b8 => Self::V22_H5,
            0x3b9 => Self::V22_H6,
            0x3ba => Self::V22_H7,
            0x3bb => Self::V23_H0,
            0x3bc => Self::V23_H1,
            0x3bd => Self::V23_H2,
            0x3be => Self::V23_H3,
            0x3bf => Self::V23_H4,
            0x3c0 => Self::V23_H5,
            0x3c1 => Self::V23_H6,
            0x3c2 => Self::V23_H7,
            0x3c3 => Self::V24_H0,
            0x3c4 => Self::V24_H1,
            0x3c5 => Self::V24_H2,
            0x3c6 => Self::V24_H3,
            0x3c7 => Self::V24_H4,
            0x3c8 => Self::V24_H5,
            0x3c9 => Self::V24_H6,
            0x3ca => Self::V24_H7,
            0x3cb => Self::V25_H0,
            0x3cc => Self::V25_H1,
            0x3cd => Self::V25_H2,
            0x3ce => Self::V25_H3,
            0x3cf => Self::V25_H4,
            0x3d0 => Self::V25_H5,
            0x3d1 => Self::V25_H6,
            0x3d2 => Self::V25_H7,
            0x3d3 => Self::V26_H0,
            0x3d4 => Self::V26_H1,
            0x3d5 => Self::V26_H2,
            0x3d6 => Self::V26_H3,
            0x3d7 => Self::V26_H4,
            0x3d8 => Self::V26_H5,
            0x3d9 => Self::V26_H6,
            0x3da => Self::V26_H7,
            0x3db => Self::V27_H0,
            0x3dc => Self::V27_H1,
            0x3dd => Self::V27_H2,
            0x3de => Self::V27_H3,
            0x3df => Self::V27_H4,
            0x3e0 => Self::V27_H5,
            0x3e1 => Self::V27_H6,
            0x3e2 => Self::V27_H7,
            0x3e3 => Self::V28_H0,
            0x3e4 => Self::V28_H1,
            0x3e5 => Self::V28_H2,
            0x3e6 => Self::V28_H3,
            0x3e7 => Self::V28_H4,
            0x3e8 => Self::V28_H5,
            0x3e9 => Self::V28_H6,
            0x3ea => Self::V28_H7,
            0x3eb => Self::V29_H0,
            0x3ec => Self::V29_H1,
            0x3ed => Self::V29_H2,
            0x3ee => Self::V29_H3,
            0x3ef => Self::V29_H4,
            0x3f0 => Self::V29_H5,
            0x3f1 => Self::V29_H6,
            0x3f2 => Self::V29_H7,
            0x3f3 => Self::V30_H0,
            0x3f4 => Self::V30_H1,
            0x3f5 => Self::V30_H2,
            0x3f6 => Self::V30_H3,
            0x3f7 => Self::V30_H4,
            0x3f8 => Self::V30_H5,
            0x3f9 => Self::V30_H6,
            0x3fa => Self::V30_H7,
            0x3fb => Self::V31_H0,
            0x3fc => Self::V31_H1,
            0x3fd => Self::V31_H2,
            0x3fe => Self::V31_H3,
            0x3ff => Self::V31_H4,
            0x400 => Self::V31_H5,
            0x401 => Self::V31_H6,
            0x402 => Self::V31_H7,
            0x403 => Self::V0_S0,
            0x404 => Self::V0_S1,
            0x405 => Self::V0_S2,
            0x406 => Self::V0_S3,
            0x407 => Self::V1_S0,
            0x408 => Self::V1_S1,
            0x409 => Self::V1_S2,
            0x40a => Self::V1_S3,
            0x40b => Self::V2_S0,
            0x40c => Self::V2_S1,
            0x40d => Self::V2_S2,
            0x40e => Self::V2_S3,
            0x40f => Self::V3_S0,
            0x410 => Self::V3_S1,
            0x411 => Self::V3_S2,
            0x412 => Self::V3_S3,
            0x413 => Self::V4_S0,
            0x414 => Self::V4_S1,
            0x415 => Self::V4_S2,
            0x416 => Self::V4_S3,
            0x417 => Self::V5_S0,
            0x418 => Self::V5_S1,
            0x419 => Self::V5_S2,
            0x41a => Self::V5_S3,
            0x41b => Self::V6_S0,
            0x41c => Self::V6_S1,
            0x41d => Self::V6_S2,
            0x41e => Self::V6_S3,
            0x41f => Self::V7_S0,
            0x420 => Self::V7_S1,
            0x421 => Self::V7_S2,
            0x422 => Self::V7_S3,
            0x423 => Self::V8_S0,
            0x424 => Self::V8_S1,
            0x425 => Self::V8_S2,
            0x426 => Self::V8_S3,
            0x427 => Self::V9_S0,
            0x428 => Self::V9_S1,
            0x429 => Self::V9_S2,
            0x42a => Self::V9_S3,
            0x42b => Self::V10_S0,
            0x42c => Self::V10_S1,
            0x42d => Self::V10_S2,
            0x42e => Self::V10_S3,
            0x42f => Self::V11_S0,
            0x430 => Self::V11_S1,
            0x431 => Self::V11_S2,
            0x432 => Self::V11_S3,
            0x433 => Self::V12_S0,
            0x434 => Self::V12_S1,
            0x435 => Self::V12_S2,
            0x436 => Self::V12_S3,
            0x437 => Self::V13_S0,
            0x438 => Self::V13_S1,
            0x439 => Self::V13_S2,
            0x43a => Self::V13_S3,
            0x43b => Self::V14_S0,
            0x43c => Self::V14_S1,
            0x43d => Self::V14_S2,
            0x43e => Self::V14_S3,
            0x43f => Self::V15_S0,
            0x440 => Self::V15_S1,
            0x441 => Self::V15_S2,
            0x442 => Self::V15_S3,
            0x443 => Self::V16_S0,
            0x444 => Self::V16_S1,
            0x445 => Self::V16_S2,
            0x446 => Self::V16_S3,
            0x447 => Self::V17_S0,
            0x448 => Self::V17_S1,
            0x449 => Self::V17_S2,
            0x44a => Self::V17_S3,
            0x44b => Self::V18_S0,
            0x44c => Self::V18_S1,
            0x44d => Self::V18_S2,
            0x44e => Self::V18_S3,
            0x44f => Self::V19_S0,
            0x450 => Self::V19_S1,
            0x451 => Self::V19_S2,
            0x452 => Self::V19_S3,
            0x453 => Self::V20_S0,
            0x454 => Self::V20_S1,
            0x455 => Self::V20_S2,
            0x456 => Self::V20_S3,
            0x457 => Self::V21_S0,
            0x458 => Self::V21_S1,
            0x459 => Self::V21_S2,
            0x45a => Self::V21_S3,
            0x45b => Self::V22_S0,
            0x45c => Self::V22_S1,
            0x45d => Self::V22_S2,
            0x45e => Self::V22_S3,
            0x45f => Self::V23_S0,
            0x460 => Self::V23_S1,
            0x461 => Self::V23_S2,
            0x462 => Self::V23_S3,
            0x463 => Self::V24_S0,
            0x464 => Self::V24_S1,
            0x465 => Self::V24_S2,
            0x466 => Self::V24_S3,
            0x467 => Self::V25_S0,
            0x468 => Self::V25_S1,
            0x469 => Self::V25_S2,
            0x46a => Self::V25_S3,
            0x46b => Self::V26_S0,
            0x46c => Self::V26_S1,
            0x46d => Self::V26_S2,
            0x46e => Self::V26_S3,
            0x46f => Self::V27_S0,
            0x470 => Self::V27_S1,
            0x471 => Self::V27_S2,
            0x472 => Self::V27_S3,
            0x473 => Self::V28_S0,
            0x474 => Self::V28_S1,
            0x475 => Self::V28_S2,
            0x476 => Self::V28_S3,
            0x477 => Self::V29_S0,
            0x478 => Self::V29_S1,
            0x479 => Self::V29_S2,
            0x47a => Self::V29_S3,
            0x47b => Self::V30_S0,
            0x47c => Self::V30_S1,
            0x47d => Self::V30_S2,
            0x47e => Self::V30_S3,
            0x47f => Self::V31_S0,
            0x480 => Self::V31_S1,
            0x481 => Self::V31_S2,
            0x482 => Self::V31_S3,
            0x483 => Self::V0_D0,
            0x484 => Self::V0_D1,
            0x485 => Self::V1_D0,
            0x486 => Self::V1_D1,
            0x487 => Self::V2_D0,
            0x488 => Self::V2_D1,
            0x489 => Self::V3_D0,
            0x48a => Self::V3_D1,
            0x48b => Self::V4_D0,
            0x48c => Self::V4_D1,
            0x48d => Self::V5_D0,
            0x48e => Self::V5_D1,
            0x48f => Self::V6_D0,
            0x490 => Self::V6_D1,
            0x491 => Self::V7_D0,
            0x492 => Self::V7_D1,
            0x493 => Self::V8_D0,
            0x494 => Self::V8_D1,
            0x495 => Self::V9_D0,
            0x496 => Self::V9_D1,
            0x497 => Self::V10_D0,
            0x498 => Self::V10_D1,
            0x499 => Self::V11_D0,
            0x49a => Self::V11_D1,
            0x49b => Self::V12_D0,
            0x49c => Self::V12_D1,
            0x49d => Self::V13_D0,
            0x49e => Self::V13_D1,
            0x49f => Self::V14_D0,
            0x4a0 => Self::V14_D1,
            0x4a1 => Self::V15_D0,
            0x4a2 => Self::V15_D1,
            0x4a3 => Self::V16_D0,
            0x4a4 => Self::V16_D1,
            0x4a5 => Self::V17_D0,
            0x4a6 => Self::V17_D1,
            0x4a7 => Self::V18_D0,
            0x4a8 => Self::V18_D1,
            0x4a9 => Self::V19_D0,
            0x4aa => Self::V19_D1,
            0x4ab => Self::V20_D0,
            0x4ac => Self::V20_D1,
            0x4ad => Self::V21_D0,
            0x4ae => Self::V21_D1,
            0x4af => Self::V22_D0,
            0x4b0 => Self::V22_D1,
            0x4b1 => Self::V23_D0,
            0x4b2 => Self::V23_D1,
            0x4b3 => Self::V24_D0,
            0x4b4 => Self::V24_D1,
            0x4b5 => Self::V25_D0,
            0x4b6 => Self::V25_D1,
            0x4b7 => Self::V26_D0,
            0x4b8 => Self::V26_D1,
            0x4b9 => Self::V27_D0,
            0x4ba => Self::V27_D1,
            0x4bb => Self::V28_D0,
            0x4bc => Self::V28_D1,
            0x4bd => Self::V29_D0,
            0x4be => Self::V29_D1,
            0x4bf => Self::V30_D0,
            0x4c0 => Self::V30_D1,
            0x4c1 => Self::V31_D0,
            0x4c2 => Self::V31_D1,
            0x4c3 => Self::Z0,
            0x4c4 => Self::Z1,
            0x4c5 => Self::Z2,
            0x4c6 => Self::Z3,
            0x4c7 => Self::Z4,
            0x4c8 => Self::Z5,
            0x4c9 => Self::Z6,
            0x4ca => Self::Z7,
            0x4cb => Self::Z8,
            0x4cc => Self::Z9,
            0x4cd => Self::Z10,
            0x4ce => Self::Z11,
            0x4cf => Self::Z12,
            0x4d0 => Self::Z13,
            0x4d1 => Self::Z14,
            0x4d2 => Self::Z15,
            0x4d3 => Self::Z16,
            0x4d4 => Self::Z17,
            0x4d5 => Self::Z18,
            0x4d6 => Self::Z19,
            0x4d7 => Self::Z20,
            0x4d8 => Self::Z21,
            0x4d9 => Self::Z22,
            0x4da => Self::Z23,
            0x4db => Self::Z24,
            0x4dc => Self::Z25,
            0x4dd => Self::Z26,
            0x4de => Self::Z27,
            0x4df => Self::Z28,
            0x4e0 => Self::Z29,
            0x4e1 => Self::Z30,
            0x4e2 => Self::Z31,
            0x4e3 => Self::P0,
            0x4e4 => Self::P1,
            0x4e5 => Self::P2,
            0x4e6 => Self::P3,
            0x4e7 => Self::P4,
            0x4e8 => Self::P5,
            0x4e9 => Self::P6,
            0x4ea => Self::P7,
            0x4eb => Self::P8,
            0x4ec => Self::P9,
            0x4ed => Self::P10,
            0x4ee => Self::P11,
            0x4ef => Self::P12,
            0x4f0 => Self::P13,
            0x4f1 => Self::P14,
            0x4f2 => Self::P15,
            0x4f3 => Self::P16,
            0x4f4 => Self::P17,
            0x4f5 => Self::P18,
            0x4f6 => Self::P19,
            0x4f7 => Self::P20,
            0x4f8 => Self::P21,
            0x4f9 => Self::P22,
            0x4fa => Self::P23,
            0x4fb => Self::P24,
            0x4fc => Self::P25,
            0x4fd => Self::P26,
            0x4fe => Self::P27,
            0x4ff => Self::P28,
            0x500 => Self::P29,
            0x501 => Self::P30,
            0x502 => Self::P31,
            0x503 => Self::PF0,
            0x504 => Self::PF1,
            0x505 => Self::PF2,
            0x506 => Self::PF3,
            0x507 => Self::PF4,
            0x508 => Self::PF5,
            0x509 => Self::PF6,
            0x50a => Self::PF7,
            0x50b => Self::PF8,
            0x50c => Self::PF9,
            0x50d => Self::PF10,
            0x50e => Self::PF11,
            0x50f => Self::PF12,
            0x510 => Self::PF13,
            0x511 => Self::PF14,
            0x512 => Self::PF15,
            0x513 => Self::PF16,
            0x514 => Self::PF17,
            0x515 => Self::PF18,
            0x516 => Self::PF19,
            0x517 => Self::PF20,
            0x518 => Self::PF21,
            0x519 => Self::PF22,
            0x51a => Self::PF23,
            0x51b => Self::PF24,
            0x51c => Self::PF25,
            0x51d => Self::PF26,
            0x51e => Self::PF27,
            0x51f => Self::PF28,
            0x520 => Self::PF29,
            0x521 => Self::PF30,
            0x522 => Self::PF31,
            0x523 => Self::END,

            _ => panic!("Invalid register value"),
        }
    }
}

impl LowerHex for RegistersType {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        write!(f, "{:x}", *self as usize)
    }
}

impl UpperHex for RegistersType {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        write!(f, "{:X}", *self as usize)
    }
}

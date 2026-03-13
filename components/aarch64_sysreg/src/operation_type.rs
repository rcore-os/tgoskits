use core::fmt::{Display, Formatter, LowerHex, Result, UpperHex};

/// Arm64 Operation type
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OperationType {
    /// Arm64 Operation ERROR
    ERROR = 0x0,
    /// Arm64 Operation ABS
    ABS = 0x1,
    /// Arm64 Operation ADC
    ADC = 0x2,
    /// Arm64 Operation ADCLB
    ADCLB = 0x3,
    /// Arm64 Operation ADCLT
    ADCLT = 0x4,
    /// Arm64 Operation ADCS
    ADCS = 0x5,
    /// Arm64 Operation ADD
    ADD = 0x6,
    /// Arm64 Operation ADDG
    ADDG = 0x7,
    /// Arm64 Operation ADDHA
    ADDHA = 0x8,
    /// Arm64 Operation ADDHN
    ADDHN = 0x9,
    /// Arm64 Operation ADDHN2
    ADDHN2 = 0xa,
    /// Arm64 Operation ADDHNB
    ADDHNB = 0xb,
    /// Arm64 Operation ADDHNT
    ADDHNT = 0xc,
    /// Arm64 Operation ADDP
    ADDP = 0xd,
    /// Arm64 Operation ADDPL
    ADDPL = 0xe,
    /// Arm64 Operation ADDS
    ADDS = 0xf,
    /// Arm64 Operation ADDV
    ADDV = 0x10,
    /// Arm64 Operation ADDVA
    ADDVA = 0x11,
    /// Arm64 Operation ADDVL
    ADDVL = 0x12,
    /// Arm64 Operation ADR
    ADR = 0x13,
    /// Arm64 Operation ADRP
    ADRP = 0x14,
    /// Arm64 Operation AESD
    AESD = 0x15,
    /// Arm64 Operation AESE
    AESE = 0x16,
    /// Arm64 Operation AESIMC
    AESIMC = 0x17,
    /// Arm64 Operation AESMC
    AESMC = 0x18,
    /// Arm64 Operation AND
    AND = 0x19,
    /// Arm64 Operation ANDS
    ANDS = 0x1a,
    /// Arm64 Operation ANDV
    ANDV = 0x1b,
    /// Arm64 Operation ASR
    ASR = 0x1c,
    /// Arm64 Operation ASRD
    ASRD = 0x1d,
    /// Arm64 Operation ASRR
    ASRR = 0x1e,
    /// Arm64 Operation ASRV
    ASRV = 0x1f,
    /// Arm64 Operation AT
    AT = 0x20,
    /// Arm64 Operation AUTDA
    AUTDA = 0x21,
    /// Arm64 Operation AUTDB
    AUTDB = 0x22,
    /// Arm64 Operation AUTDZA
    AUTDZA = 0x23,
    /// Arm64 Operation AUTDZB
    AUTDZB = 0x24,
    /// Arm64 Operation AUTIA
    AUTIA = 0x25,
    /// Arm64 Operation AUTIA1716
    AUTIA1716 = 0x26,
    /// Arm64 Operation AUTIASP
    AUTIASP = 0x27,
    /// Arm64 Operation AUTIAZ
    AUTIAZ = 0x28,
    /// Arm64 Operation AUTIB
    AUTIB = 0x29,
    /// Arm64 Operation AUTIB1716
    AUTIB1716 = 0x2a,
    /// Arm64 Operation AUTIBSP
    AUTIBSP = 0x2b,
    /// Arm64 Operation AUTIBZ
    AUTIBZ = 0x2c,
    /// Arm64 Operation AUTIZA
    AUTIZA = 0x2d,
    /// Arm64 Operation AUTIZB
    AUTIZB = 0x2e,
    /// Arm64 Operation AXFLAG
    AXFLAG = 0x2f,
    /// Arm64 Operation B
    B = 0x30,
    /// Arm64 Operation BCAX
    BCAX = 0x31,
    /// Arm64 Operation BDEP
    BDEP = 0x32,
    /// Arm64 Operation BEXT
    BEXT = 0x33,
    /// Arm64 Operation BFC
    BFC = 0x34,
    /// Arm64 Operation BFCVT
    BFCVT = 0x35,
    /// Arm64 Operation BFCVTN
    BFCVTN = 0x36,
    /// Arm64 Operation BFCVTN2
    BFCVTN2 = 0x37,
    /// Arm64 Operation BFCVTNT
    BFCVTNT = 0x38,
    /// Arm64 Operation BFDOT
    BFDOT = 0x39,
    /// Arm64 Operation BFI
    BFI = 0x3a,
    /// Arm64 Operation BFM
    BFM = 0x3b,
    /// Arm64 Operation BFMLAL
    BFMLAL = 0x3c,
    /// Arm64 Operation BFMLALB
    BFMLALB = 0x3d,
    /// Arm64 Operation BFMLALT
    BFMLALT = 0x3e,
    /// Arm64 Operation BFMMLA
    BFMMLA = 0x3f,
    /// Arm64 Operation BFMOPA
    BFMOPA = 0x40,
    /// Arm64 Operation BFMOPS
    BFMOPS = 0x41,
    /// Arm64 Operation BFXIL
    BFXIL = 0x42,
    /// Arm64 Operation BGRP
    BGRP = 0x43,
    /// Arm64 Operation BIC
    BIC = 0x44,
    /// Arm64 Operation BICS
    BICS = 0x45,
    /// Arm64 Operation BIF
    BIF = 0x46,
    /// Arm64 Operation BIT
    BIT = 0x47,
    /// Arm64 Operation BL
    BL = 0x48,
    /// Arm64 Operation BLR
    BLR = 0x49,
    /// Arm64 Operation BLRAA
    BLRAA = 0x4a,
    /// Arm64 Operation BLRAAZ
    BLRAAZ = 0x4b,
    /// Arm64 Operation BLRAB
    BLRAB = 0x4c,
    /// Arm64 Operation BLRABZ
    BLRABZ = 0x4d,
    /// Arm64 Operation BR
    BR = 0x4e,
    /// Arm64 Operation BRAA
    BRAA = 0x4f,
    /// Arm64 Operation BRAAZ
    BRAAZ = 0x50,
    /// Arm64 Operation BRAB
    BRAB = 0x51,
    /// Arm64 Operation BRABZ
    BRABZ = 0x52,
    /// Arm64 Operation BRK
    BRK = 0x53,
    /// Arm64 Operation BRKA
    BRKA = 0x54,
    /// Arm64 Operation BRKAS
    BRKAS = 0x55,
    /// Arm64 Operation BRKB
    BRKB = 0x56,
    /// Arm64 Operation BRKBS
    BRKBS = 0x57,
    /// Arm64 Operation BRKN
    BRKN = 0x58,
    /// Arm64 Operation BRKNS
    BRKNS = 0x59,
    /// Arm64 Operation BRKPA
    BRKPA = 0x5a,
    /// Arm64 Operation BRKPAS
    BRKPAS = 0x5b,
    /// Arm64 Operation BRKPB
    BRKPB = 0x5c,
    /// Arm64 Operation BRKPBS
    BRKPBS = 0x5d,
    /// Arm64 Operation BSL
    BSL = 0x5e,
    /// Arm64 Operation BSL1N
    BSL1N = 0x5f,
    /// Arm64 Operation BSL2N
    BSL2N = 0x60,
    /// Arm64 Operation BTI
    BTI = 0x61,
    /// Arm64 Operation B_AL
    B_AL = 0x62,
    /// Arm64 Operation B_CC
    B_CC = 0x63,
    /// Arm64 Operation B_CS
    B_CS = 0x64,
    /// Arm64 Operation B_EQ
    B_EQ = 0x65,
    /// Arm64 Operation B_GE
    B_GE = 0x66,
    /// Arm64 Operation B_GT
    B_GT = 0x67,
    /// Arm64 Operation B_HI
    B_HI = 0x68,
    /// Arm64 Operation B_LE
    B_LE = 0x69,
    /// Arm64 Operation B_LS
    B_LS = 0x6a,
    /// Arm64 Operation B_LT
    B_LT = 0x6b,
    /// Arm64 Operation B_MI
    B_MI = 0x6c,
    /// Arm64 Operation B_NE
    B_NE = 0x6d,
    /// Arm64 Operation B_NV
    B_NV = 0x6e,
    /// Arm64 Operation B_PL
    B_PL = 0x6f,
    /// Arm64 Operation B_VC
    B_VC = 0x70,
    /// Arm64 Operation B_VS
    B_VS = 0x71,
    /// Arm64 Operation CADD
    CADD = 0x72,
    /// Arm64 Operation CAS
    CAS = 0x73,
    /// Arm64 Operation CASA
    CASA = 0x74,
    /// Arm64 Operation CASAB
    CASAB = 0x75,
    /// Arm64 Operation CASAH
    CASAH = 0x76,
    /// Arm64 Operation CASAL
    CASAL = 0x77,
    /// Arm64 Operation CASALB
    CASALB = 0x78,
    /// Arm64 Operation CASALH
    CASALH = 0x79,
    /// Arm64 Operation CASB
    CASB = 0x7a,
    /// Arm64 Operation CASH
    CASH = 0x7b,
    /// Arm64 Operation CASL
    CASL = 0x7c,
    /// Arm64 Operation CASLB
    CASLB = 0x7d,
    /// Arm64 Operation CASLH
    CASLH = 0x7e,
    /// Arm64 Operation CASP
    CASP = 0x7f,
    /// Arm64 Operation CASPA
    CASPA = 0x80,
    /// Arm64 Operation CASPAL
    CASPAL = 0x81,
    /// Arm64 Operation CASPL
    CASPL = 0x82,
    /// Arm64 Operation CBNZ
    CBNZ = 0x83,
    /// Arm64 Operation CBZ
    CBZ = 0x84,
    /// Arm64 Operation CCMN
    CCMN = 0x85,
    /// Arm64 Operation CCMP
    CCMP = 0x86,
    /// Arm64 Operation CDOT
    CDOT = 0x87,
    /// Arm64 Operation CFINV
    CFINV = 0x88,
    /// Arm64 Operation CFP
    CFP = 0x89,
    /// Arm64 Operation CINC
    CINC = 0x8a,
    /// Arm64 Operation CINV
    CINV = 0x8b,
    /// Arm64 Operation CLASTA
    CLASTA = 0x8c,
    /// Arm64 Operation CLASTB
    CLASTB = 0x8d,
    /// Arm64 Operation CLREX
    CLREX = 0x8e,
    /// Arm64 Operation CLS
    CLS = 0x8f,
    /// Arm64 Operation CLZ
    CLZ = 0x90,
    /// Arm64 Operation CMEQ
    CMEQ = 0x91,
    /// Arm64 Operation CMGE
    CMGE = 0x92,
    /// Arm64 Operation CMGT
    CMGT = 0x93,
    /// Arm64 Operation CMHI
    CMHI = 0x94,
    /// Arm64 Operation CMHS
    CMHS = 0x95,
    /// Arm64 Operation CMLA
    CMLA = 0x96,
    /// Arm64 Operation CMLE
    CMLE = 0x97,
    /// Arm64 Operation CMLT
    CMLT = 0x98,
    /// Arm64 Operation CMN
    CMN = 0x99,
    /// Arm64 Operation CMP
    CMP = 0x9a,
    /// Arm64 Operation CMPEQ
    CMPEQ = 0x9b,
    /// Arm64 Operation CMPGE
    CMPGE = 0x9c,
    /// Arm64 Operation CMPGT
    CMPGT = 0x9d,
    /// Arm64 Operation CMPHI
    CMPHI = 0x9e,
    /// Arm64 Operation CMPHS
    CMPHS = 0x9f,
    /// Arm64 Operation CMPLE
    CMPLE = 0xa0,
    /// Arm64 Operation CMPLO
    CMPLO = 0xa1,
    /// Arm64 Operation CMPLS
    CMPLS = 0xa2,
    /// Arm64 Operation CMPLT
    CMPLT = 0xa3,
    /// Arm64 Operation CMPNE
    CMPNE = 0xa4,
    /// Arm64 Operation CMPP
    CMPP = 0xa5,
    /// Arm64 Operation CMTST
    CMTST = 0xa6,
    /// Arm64 Operation CNEG
    CNEG = 0xa7,
    /// Arm64 Operation CNOT
    CNOT = 0xa8,
    /// Arm64 Operation CNT
    CNT = 0xa9,
    /// Arm64 Operation CNTB
    CNTB = 0xaa,
    /// Arm64 Operation CNTD
    CNTD = 0xab,
    /// Arm64 Operation CNTH
    CNTH = 0xac,
    /// Arm64 Operation CNTP
    CNTP = 0xad,
    /// Arm64 Operation CNTW
    CNTW = 0xae,
    /// Arm64 Operation COMPACT
    COMPACT = 0xaf,
    /// Arm64 Operation CPP
    CPP = 0xb0,
    /// Arm64 Operation CPY
    CPY = 0xb1,
    /// Arm64 Operation CRC32B
    CRC32B = 0xb2,
    /// Arm64 Operation CRC32CB
    CRC32CB = 0xb3,
    /// Arm64 Operation CRC32CH
    CRC32CH = 0xb4,
    /// Arm64 Operation CRC32CW
    CRC32CW = 0xb5,
    /// Arm64 Operation CRC32CX
    CRC32CX = 0xb6,
    /// Arm64 Operation CRC32H
    CRC32H = 0xb7,
    /// Arm64 Operation CRC32W
    CRC32W = 0xb8,
    /// Arm64 Operation CRC32X
    CRC32X = 0xb9,
    /// Arm64 Operation CSDB
    CSDB = 0xba,
    /// Arm64 Operation CSEL
    CSEL = 0xbb,
    /// Arm64 Operation CSET
    CSET = 0xbc,
    /// Arm64 Operation CSETM
    CSETM = 0xbd,
    /// Arm64 Operation CSINC
    CSINC = 0xbe,
    /// Arm64 Operation CSINV
    CSINV = 0xbf,
    /// Arm64 Operation CSNEG
    CSNEG = 0xc0,
    /// Arm64 Operation CTERMEQ
    CTERMEQ = 0xc1,
    /// Arm64 Operation CTERMNE
    CTERMNE = 0xc2,
    /// Arm64 Operation DC
    DC = 0xc3,
    /// Arm64 Operation DCPS1
    DCPS1 = 0xc4,
    /// Arm64 Operation DCPS2
    DCPS2 = 0xc5,
    /// Arm64 Operation DCPS3
    DCPS3 = 0xc6,
    /// Arm64 Operation DECB
    DECB = 0xc7,
    /// Arm64 Operation DECD
    DECD = 0xc8,
    /// Arm64 Operation DECH
    DECH = 0xc9,
    /// Arm64 Operation DECP
    DECP = 0xca,
    /// Arm64 Operation DECW
    DECW = 0xcb,
    /// Arm64 Operation DGH
    DGH = 0xcc,
    /// Arm64 Operation DMB
    DMB = 0xcd,
    /// Arm64 Operation DRPS
    DRPS = 0xce,
    /// Arm64 Operation DSB
    DSB = 0xcf,
    /// Arm64 Operation DUP
    DUP = 0xd0,
    /// Arm64 Operation DUPM
    DUPM = 0xd1,
    /// Arm64 Operation DVP
    DVP = 0xd2,
    /// Arm64 Operation EON
    EON = 0xd3,
    /// Arm64 Operation EOR
    EOR = 0xd4,
    /// Arm64 Operation EOR3
    EOR3 = 0xd5,
    /// Arm64 Operation EORBT
    EORBT = 0xd6,
    /// Arm64 Operation EORS
    EORS = 0xd7,
    /// Arm64 Operation EORTB
    EORTB = 0xd8,
    /// Arm64 Operation EORV
    EORV = 0xd9,
    /// Arm64 Operation ERET
    ERET = 0xda,
    /// Arm64 Operation ERETAA
    ERETAA = 0xdb,
    /// Arm64 Operation ERETAB
    ERETAB = 0xdc,
    /// Arm64 Operation ESB
    ESB = 0xdd,
    /// Arm64 Operation EXT
    EXT = 0xde,
    /// Arm64 Operation EXTR
    EXTR = 0xdf,
    /// Arm64 Operation FABD
    FABD = 0xe0,
    /// Arm64 Operation FABS
    FABS = 0xe1,
    /// Arm64 Operation FACGE
    FACGE = 0xe2,
    /// Arm64 Operation FACGT
    FACGT = 0xe3,
    /// Arm64 Operation FACLE
    FACLE = 0xe4,
    /// Arm64 Operation FACLT
    FACLT = 0xe5,
    /// Arm64 Operation FADD
    FADD = 0xe6,
    /// Arm64 Operation FADDA
    FADDA = 0xe7,
    /// Arm64 Operation FADDP
    FADDP = 0xe8,
    /// Arm64 Operation FADDV
    FADDV = 0xe9,
    /// Arm64 Operation FCADD
    FCADD = 0xea,
    /// Arm64 Operation FCCMP
    FCCMP = 0xeb,
    /// Arm64 Operation FCCMPE
    FCCMPE = 0xec,
    /// Arm64 Operation FCMEQ
    FCMEQ = 0xed,
    /// Arm64 Operation FCMGE
    FCMGE = 0xee,
    /// Arm64 Operation FCMGT
    FCMGT = 0xef,
    /// Arm64 Operation FCMLA
    FCMLA = 0xf0,
    /// Arm64 Operation FCMLE
    FCMLE = 0xf1,
    /// Arm64 Operation FCMLT
    FCMLT = 0xf2,
    /// Arm64 Operation FCMNE
    FCMNE = 0xf3,
    /// Arm64 Operation FCMP
    FCMP = 0xf4,
    /// Arm64 Operation FCMPE
    FCMPE = 0xf5,
    /// Arm64 Operation FCMUO
    FCMUO = 0xf6,
    /// Arm64 Operation FCPY
    FCPY = 0xf7,
    /// Arm64 Operation FCSEL
    FCSEL = 0xf8,
    /// Arm64 Operation FCVT
    FCVT = 0xf9,
    /// Arm64 Operation FCVTAS
    FCVTAS = 0xfa,
    /// Arm64 Operation FCVTAU
    FCVTAU = 0xfb,
    /// Arm64 Operation FCVTL
    FCVTL = 0xfc,
    /// Arm64 Operation FCVTL2
    FCVTL2 = 0xfd,
    /// Arm64 Operation FCVTLT
    FCVTLT = 0xfe,
    /// Arm64 Operation FCVTMS
    FCVTMS = 0xff,
    /// Arm64 Operation FCVTMU
    FCVTMU = 0x100,
    /// Arm64 Operation FCVTN
    FCVTN = 0x101,
    /// Arm64 Operation FCVTN2
    FCVTN2 = 0x102,
    /// Arm64 Operation FCVTNS
    FCVTNS = 0x103,
    /// Arm64 Operation FCVTNT
    FCVTNT = 0x104,
    /// Arm64 Operation FCVTNU
    FCVTNU = 0x105,
    /// Arm64 Operation FCVTPS
    FCVTPS = 0x106,
    /// Arm64 Operation FCVTPU
    FCVTPU = 0x107,
    /// Arm64 Operation FCVTX
    FCVTX = 0x108,
    /// Arm64 Operation FCVTXN
    FCVTXN = 0x109,
    /// Arm64 Operation FCVTXN2
    FCVTXN2 = 0x10a,
    /// Arm64 Operation FCVTXNT
    FCVTXNT = 0x10b,
    /// Arm64 Operation FCVTZS
    FCVTZS = 0x10c,
    /// Arm64 Operation FCVTZU
    FCVTZU = 0x10d,
    /// Arm64 Operation FDIV
    FDIV = 0x10e,
    /// Arm64 Operation FDIVR
    FDIVR = 0x10f,
    /// Arm64 Operation FDUP
    FDUP = 0x110,
    /// Arm64 Operation FEXPA
    FEXPA = 0x111,
    /// Arm64 Operation FJCVTZS
    FJCVTZS = 0x112,
    /// Arm64 Operation FLOGB
    FLOGB = 0x113,
    /// Arm64 Operation FMAD
    FMAD = 0x114,
    /// Arm64 Operation FMADD
    FMADD = 0x115,
    /// Arm64 Operation FMAX
    FMAX = 0x116,
    /// Arm64 Operation FMAXNM
    FMAXNM = 0x117,
    /// Arm64 Operation FMAXNMP
    FMAXNMP = 0x118,
    /// Arm64 Operation FMAXNMV
    FMAXNMV = 0x119,
    /// Arm64 Operation FMAXP
    FMAXP = 0x11a,
    /// Arm64 Operation FMAXV
    FMAXV = 0x11b,
    /// Arm64 Operation FMIN
    FMIN = 0x11c,
    /// Arm64 Operation FMINNM
    FMINNM = 0x11d,
    /// Arm64 Operation FMINNMP
    FMINNMP = 0x11e,
    /// Arm64 Operation FMINNMV
    FMINNMV = 0x11f,
    /// Arm64 Operation FMINP
    FMINP = 0x120,
    /// Arm64 Operation FMINV
    FMINV = 0x121,
    /// Arm64 Operation FMLA
    FMLA = 0x122,
    /// Arm64 Operation FMLAL
    FMLAL = 0x123,
    /// Arm64 Operation FMLAL2
    FMLAL2 = 0x124,
    /// Arm64 Operation FMLALB
    FMLALB = 0x125,
    /// Arm64 Operation FMLALT
    FMLALT = 0x126,
    /// Arm64 Operation FMLS
    FMLS = 0x127,
    /// Arm64 Operation FMLSL
    FMLSL = 0x128,
    /// Arm64 Operation FMLSL2
    FMLSL2 = 0x129,
    /// Arm64 Operation FMLSLB
    FMLSLB = 0x12a,
    /// Arm64 Operation FMLSLT
    FMLSLT = 0x12b,
    /// Arm64 Operation FMMLA
    FMMLA = 0x12c,
    /// Arm64 Operation FMOPA
    FMOPA = 0x12d,
    /// Arm64 Operation FMOPS
    FMOPS = 0x12e,
    /// Arm64 Operation FMOV
    FMOV = 0x12f,
    /// Arm64 Operation FMSB
    FMSB = 0x130,
    /// Arm64 Operation FMSUB
    FMSUB = 0x131,
    /// Arm64 Operation FMUL
    FMUL = 0x132,
    /// Arm64 Operation FMULX
    FMULX = 0x133,
    /// Arm64 Operation FNEG
    FNEG = 0x134,
    /// Arm64 Operation FNMAD
    FNMAD = 0x135,
    /// Arm64 Operation FNMADD
    FNMADD = 0x136,
    /// Arm64 Operation FNMLA
    FNMLA = 0x137,
    /// Arm64 Operation FNMLS
    FNMLS = 0x138,
    /// Arm64 Operation FNMSB
    FNMSB = 0x139,
    /// Arm64 Operation FNMSUB
    FNMSUB = 0x13a,
    /// Arm64 Operation FNMUL
    FNMUL = 0x13b,
    /// Arm64 Operation FRECPE
    FRECPE = 0x13c,
    /// Arm64 Operation FRECPS
    FRECPS = 0x13d,
    /// Arm64 Operation FRECPX
    FRECPX = 0x13e,
    /// Arm64 Operation FRINT32X
    FRINT32X = 0x13f,
    /// Arm64 Operation FRINT32Z
    FRINT32Z = 0x140,
    /// Arm64 Operation FRINT64X
    FRINT64X = 0x141,
    /// Arm64 Operation FRINT64Z
    FRINT64Z = 0x142,
    /// Arm64 Operation FRINTA
    FRINTA = 0x143,
    /// Arm64 Operation FRINTI
    FRINTI = 0x144,
    /// Arm64 Operation FRINTM
    FRINTM = 0x145,
    /// Arm64 Operation FRINTN
    FRINTN = 0x146,
    /// Arm64 Operation FRINTP
    FRINTP = 0x147,
    /// Arm64 Operation FRINTX
    FRINTX = 0x148,
    /// Arm64 Operation FRINTZ
    FRINTZ = 0x149,
    /// Arm64 Operation FRSQRTE
    FRSQRTE = 0x14a,
    /// Arm64 Operation FRSQRTS
    FRSQRTS = 0x14b,
    /// Arm64 Operation FSCALE
    FSCALE = 0x14c,
    /// Arm64 Operation FSQRT
    FSQRT = 0x14d,
    /// Arm64 Operation FSUB
    FSUB = 0x14e,
    /// Arm64 Operation FSUBR
    FSUBR = 0x14f,
    /// Arm64 Operation FTMAD
    FTMAD = 0x150,
    /// Arm64 Operation FTSMUL
    FTSMUL = 0x151,
    /// Arm64 Operation FTSSEL
    FTSSEL = 0x152,
    /// Arm64 Operation GMI
    GMI = 0x153,
    /// Arm64 Operation HINT
    HINT = 0x154,
    /// Arm64 Operation HISTCNT
    HISTCNT = 0x155,
    /// Arm64 Operation HISTSEG
    HISTSEG = 0x156,
    /// Arm64 Operation HLT
    HLT = 0x157,
    /// Arm64 Operation HVC
    HVC = 0x158,
    /// Arm64 Operation IC
    IC = 0x159,
    /// Arm64 Operation INCB
    INCB = 0x15a,
    /// Arm64 Operation INCD
    INCD = 0x15b,
    /// Arm64 Operation INCH
    INCH = 0x15c,
    /// Arm64 Operation INCP
    INCP = 0x15d,
    /// Arm64 Operation INCW
    INCW = 0x15e,
    /// Arm64 Operation INDEX
    INDEX = 0x15f,
    /// Arm64 Operation INS
    INS = 0x160,
    /// Arm64 Operation INSR
    INSR = 0x161,
    /// Arm64 Operation IRG
    IRG = 0x162,
    /// Arm64 Operation ISB
    ISB = 0x163,
    /// Arm64 Operation LASTA
    LASTA = 0x164,
    /// Arm64 Operation LASTB
    LASTB = 0x165,
    /// Arm64 Operation LD1
    LD1 = 0x166,
    /// Arm64 Operation LD1B
    LD1B = 0x167,
    /// Arm64 Operation LD1D
    LD1D = 0x168,
    /// Arm64 Operation LD1H
    LD1H = 0x169,
    /// Arm64 Operation LD1Q
    LD1Q = 0x16a,
    /// Arm64 Operation LD1R
    LD1R = 0x16b,
    /// Arm64 Operation LD1RB
    LD1RB = 0x16c,
    /// Arm64 Operation LD1RD
    LD1RD = 0x16d,
    /// Arm64 Operation LD1RH
    LD1RH = 0x16e,
    /// Arm64 Operation LD1ROB
    LD1ROB = 0x16f,
    /// Arm64 Operation LD1ROD
    LD1ROD = 0x170,
    /// Arm64 Operation LD1ROH
    LD1ROH = 0x171,
    /// Arm64 Operation LD1ROW
    LD1ROW = 0x172,
    /// Arm64 Operation LD1RQB
    LD1RQB = 0x173,
    /// Arm64 Operation LD1RQD
    LD1RQD = 0x174,
    /// Arm64 Operation LD1RQH
    LD1RQH = 0x175,
    /// Arm64 Operation LD1RQW
    LD1RQW = 0x176,
    /// Arm64 Operation LD1RSB
    LD1RSB = 0x177,
    /// Arm64 Operation LD1RSH
    LD1RSH = 0x178,
    /// Arm64 Operation LD1RSW
    LD1RSW = 0x179,
    /// Arm64 Operation LD1RW
    LD1RW = 0x17a,
    /// Arm64 Operation LD1SB
    LD1SB = 0x17b,
    /// Arm64 Operation LD1SH
    LD1SH = 0x17c,
    /// Arm64 Operation LD1SW
    LD1SW = 0x17d,
    /// Arm64 Operation LD1W
    LD1W = 0x17e,
    /// Arm64 Operation LD2
    LD2 = 0x17f,
    /// Arm64 Operation LD2B
    LD2B = 0x180,
    /// Arm64 Operation LD2D
    LD2D = 0x181,
    /// Arm64 Operation LD2H
    LD2H = 0x182,
    /// Arm64 Operation LD2R
    LD2R = 0x183,
    /// Arm64 Operation LD2W
    LD2W = 0x184,
    /// Arm64 Operation LD3
    LD3 = 0x185,
    /// Arm64 Operation LD3B
    LD3B = 0x186,
    /// Arm64 Operation LD3D
    LD3D = 0x187,
    /// Arm64 Operation LD3H
    LD3H = 0x188,
    /// Arm64 Operation LD3R
    LD3R = 0x189,
    /// Arm64 Operation LD3W
    LD3W = 0x18a,
    /// Arm64 Operation LD4
    LD4 = 0x18b,
    /// Arm64 Operation LD4B
    LD4B = 0x18c,
    /// Arm64 Operation LD4D
    LD4D = 0x18d,
    /// Arm64 Operation LD4H
    LD4H = 0x18e,
    /// Arm64 Operation LD4R
    LD4R = 0x18f,
    /// Arm64 Operation LD4W
    LD4W = 0x190,
    /// Arm64 Operation LD64B
    LD64B = 0x191,
    /// Arm64 Operation LDADD
    LDADD = 0x192,
    /// Arm64 Operation LDADDA
    LDADDA = 0x193,
    /// Arm64 Operation LDADDAB
    LDADDAB = 0x194,
    /// Arm64 Operation LDADDAH
    LDADDAH = 0x195,
    /// Arm64 Operation LDADDAL
    LDADDAL = 0x196,
    /// Arm64 Operation LDADDALB
    LDADDALB = 0x197,
    /// Arm64 Operation LDADDALH
    LDADDALH = 0x198,
    /// Arm64 Operation LDADDB
    LDADDB = 0x199,
    /// Arm64 Operation LDADDH
    LDADDH = 0x19a,
    /// Arm64 Operation LDADDL
    LDADDL = 0x19b,
    /// Arm64 Operation LDADDLB
    LDADDLB = 0x19c,
    /// Arm64 Operation LDADDLH
    LDADDLH = 0x19d,
    /// Arm64 Operation LDAPR
    LDAPR = 0x19e,
    /// Arm64 Operation LDAPRB
    LDAPRB = 0x19f,
    /// Arm64 Operation LDAPRH
    LDAPRH = 0x1a0,
    /// Arm64 Operation LDAPUR
    LDAPUR = 0x1a1,
    /// Arm64 Operation LDAPURB
    LDAPURB = 0x1a2,
    /// Arm64 Operation LDAPURH
    LDAPURH = 0x1a3,
    /// Arm64 Operation LDAPURSB
    LDAPURSB = 0x1a4,
    /// Arm64 Operation LDAPURSH
    LDAPURSH = 0x1a5,
    /// Arm64 Operation LDAPURSW
    LDAPURSW = 0x1a6,
    /// Arm64 Operation LDAR
    LDAR = 0x1a7,
    /// Arm64 Operation LDARB
    LDARB = 0x1a8,
    /// Arm64 Operation LDARH
    LDARH = 0x1a9,
    /// Arm64 Operation LDAXP
    LDAXP = 0x1aa,
    /// Arm64 Operation LDAXR
    LDAXR = 0x1ab,
    /// Arm64 Operation LDAXRB
    LDAXRB = 0x1ac,
    /// Arm64 Operation LDAXRH
    LDAXRH = 0x1ad,
    /// Arm64 Operation LDCLR
    LDCLR = 0x1ae,
    /// Arm64 Operation LDCLRA
    LDCLRA = 0x1af,
    /// Arm64 Operation LDCLRAB
    LDCLRAB = 0x1b0,
    /// Arm64 Operation LDCLRAH
    LDCLRAH = 0x1b1,
    /// Arm64 Operation LDCLRAL
    LDCLRAL = 0x1b2,
    /// Arm64 Operation LDCLRALB
    LDCLRALB = 0x1b3,
    /// Arm64 Operation LDCLRALH
    LDCLRALH = 0x1b4,
    /// Arm64 Operation LDCLRB
    LDCLRB = 0x1b5,
    /// Arm64 Operation LDCLRH
    LDCLRH = 0x1b6,
    /// Arm64 Operation LDCLRL
    LDCLRL = 0x1b7,
    /// Arm64 Operation LDCLRLB
    LDCLRLB = 0x1b8,
    /// Arm64 Operation LDCLRLH
    LDCLRLH = 0x1b9,
    /// Arm64 Operation LDEOR
    LDEOR = 0x1ba,
    /// Arm64 Operation LDEORA
    LDEORA = 0x1bb,
    /// Arm64 Operation LDEORAB
    LDEORAB = 0x1bc,
    /// Arm64 Operation LDEORAH
    LDEORAH = 0x1bd,
    /// Arm64 Operation LDEORAL
    LDEORAL = 0x1be,
    /// Arm64 Operation LDEORALB
    LDEORALB = 0x1bf,
    /// Arm64 Operation LDEORALH
    LDEORALH = 0x1c0,
    /// Arm64 Operation LDEORB
    LDEORB = 0x1c1,
    /// Arm64 Operation LDEORH
    LDEORH = 0x1c2,
    /// Arm64 Operation LDEORL
    LDEORL = 0x1c3,
    /// Arm64 Operation LDEORLB
    LDEORLB = 0x1c4,
    /// Arm64 Operation LDEORLH
    LDEORLH = 0x1c5,
    /// Arm64 Operation LDFF1B
    LDFF1B = 0x1c6,
    /// Arm64 Operation LDFF1D
    LDFF1D = 0x1c7,
    /// Arm64 Operation LDFF1H
    LDFF1H = 0x1c8,
    /// Arm64 Operation LDFF1SB
    LDFF1SB = 0x1c9,
    /// Arm64 Operation LDFF1SH
    LDFF1SH = 0x1ca,
    /// Arm64 Operation LDFF1SW
    LDFF1SW = 0x1cb,
    /// Arm64 Operation LDFF1W
    LDFF1W = 0x1cc,
    /// Arm64 Operation LDG
    LDG = 0x1cd,
    /// Arm64 Operation LDGM
    LDGM = 0x1ce,
    /// Arm64 Operation LDLAR
    LDLAR = 0x1cf,
    /// Arm64 Operation LDLARB
    LDLARB = 0x1d0,
    /// Arm64 Operation LDLARH
    LDLARH = 0x1d1,
    /// Arm64 Operation LDNF1B
    LDNF1B = 0x1d2,
    /// Arm64 Operation LDNF1D
    LDNF1D = 0x1d3,
    /// Arm64 Operation LDNF1H
    LDNF1H = 0x1d4,
    /// Arm64 Operation LDNF1SB
    LDNF1SB = 0x1d5,
    /// Arm64 Operation LDNF1SH
    LDNF1SH = 0x1d6,
    /// Arm64 Operation LDNF1SW
    LDNF1SW = 0x1d7,
    /// Arm64 Operation LDNF1W
    LDNF1W = 0x1d8,
    /// Arm64 Operation LDNP
    LDNP = 0x1d9,
    /// Arm64 Operation LDNT1B
    LDNT1B = 0x1da,
    /// Arm64 Operation LDNT1D
    LDNT1D = 0x1db,
    /// Arm64 Operation LDNT1H
    LDNT1H = 0x1dc,
    /// Arm64 Operation LDNT1SB
    LDNT1SB = 0x1dd,
    /// Arm64 Operation LDNT1SH
    LDNT1SH = 0x1de,
    /// Arm64 Operation LDNT1SW
    LDNT1SW = 0x1df,
    /// Arm64 Operation LDNT1W
    LDNT1W = 0x1e0,
    /// Arm64 Operation LDP
    LDP = 0x1e1,
    /// Arm64 Operation LDPSW
    LDPSW = 0x1e2,
    /// Arm64 Operation LDR
    LDR = 0x1e3,
    /// Arm64 Operation LDRAA
    LDRAA = 0x1e4,
    /// Arm64 Operation LDRAB
    LDRAB = 0x1e5,
    /// Arm64 Operation LDRB
    LDRB = 0x1e6,
    /// Arm64 Operation LDRH
    LDRH = 0x1e7,
    /// Arm64 Operation LDRSB
    LDRSB = 0x1e8,
    /// Arm64 Operation LDRSH
    LDRSH = 0x1e9,
    /// Arm64 Operation LDRSW
    LDRSW = 0x1ea,
    /// Arm64 Operation LDSET
    LDSET = 0x1eb,
    /// Arm64 Operation LDSETA
    LDSETA = 0x1ec,
    /// Arm64 Operation LDSETAB
    LDSETAB = 0x1ed,
    /// Arm64 Operation LDSETAH
    LDSETAH = 0x1ee,
    /// Arm64 Operation LDSETAL
    LDSETAL = 0x1ef,
    /// Arm64 Operation LDSETALB
    LDSETALB = 0x1f0,
    /// Arm64 Operation LDSETALH
    LDSETALH = 0x1f1,
    /// Arm64 Operation LDSETB
    LDSETB = 0x1f2,
    /// Arm64 Operation LDSETH
    LDSETH = 0x1f3,
    /// Arm64 Operation LDSETL
    LDSETL = 0x1f4,
    /// Arm64 Operation LDSETLB
    LDSETLB = 0x1f5,
    /// Arm64 Operation LDSETLH
    LDSETLH = 0x1f6,
    /// Arm64 Operation LDSMAX
    LDSMAX = 0x1f7,
    /// Arm64 Operation LDSMAXA
    LDSMAXA = 0x1f8,
    /// Arm64 Operation LDSMAXAB
    LDSMAXAB = 0x1f9,
    /// Arm64 Operation LDSMAXAH
    LDSMAXAH = 0x1fa,
    /// Arm64 Operation LDSMAXAL
    LDSMAXAL = 0x1fb,
    /// Arm64 Operation LDSMAXALB
    LDSMAXALB = 0x1fc,
    /// Arm64 Operation LDSMAXALH
    LDSMAXALH = 0x1fd,
    /// Arm64 Operation LDSMAXB
    LDSMAXB = 0x1fe,
    /// Arm64 Operation LDSMAXH
    LDSMAXH = 0x1ff,
    /// Arm64 Operation LDSMAXL
    LDSMAXL = 0x200,
    /// Arm64 Operation LDSMAXLB
    LDSMAXLB = 0x201,
    /// Arm64 Operation LDSMAXLH
    LDSMAXLH = 0x202,
    /// Arm64 Operation LDSMIN
    LDSMIN = 0x203,
    /// Arm64 Operation LDSMINA
    LDSMINA = 0x204,
    /// Arm64 Operation LDSMINAB
    LDSMINAB = 0x205,
    /// Arm64 Operation LDSMINAH
    LDSMINAH = 0x206,
    /// Arm64 Operation LDSMINAL
    LDSMINAL = 0x207,
    /// Arm64 Operation LDSMINALB
    LDSMINALB = 0x208,
    /// Arm64 Operation LDSMINALH
    LDSMINALH = 0x209,
    /// Arm64 Operation LDSMINB
    LDSMINB = 0x20a,
    /// Arm64 Operation LDSMINH
    LDSMINH = 0x20b,
    /// Arm64 Operation LDSMINL
    LDSMINL = 0x20c,
    /// Arm64 Operation LDSMINLB
    LDSMINLB = 0x20d,
    /// Arm64 Operation LDSMINLH
    LDSMINLH = 0x20e,
    /// Arm64 Operation LDTR
    LDTR = 0x20f,
    /// Arm64 Operation LDTRB
    LDTRB = 0x210,
    /// Arm64 Operation LDTRH
    LDTRH = 0x211,
    /// Arm64 Operation LDTRSB
    LDTRSB = 0x212,
    /// Arm64 Operation LDTRSH
    LDTRSH = 0x213,
    /// Arm64 Operation LDTRSW
    LDTRSW = 0x214,
    /// Arm64 Operation LDUMAX
    LDUMAX = 0x215,
    /// Arm64 Operation LDUMAXA
    LDUMAXA = 0x216,
    /// Arm64 Operation LDUMAXAB
    LDUMAXAB = 0x217,
    /// Arm64 Operation LDUMAXAH
    LDUMAXAH = 0x218,
    /// Arm64 Operation LDUMAXAL
    LDUMAXAL = 0x219,
    /// Arm64 Operation LDUMAXALB
    LDUMAXALB = 0x21a,
    /// Arm64 Operation LDUMAXALH
    LDUMAXALH = 0x21b,
    /// Arm64 Operation LDUMAXB
    LDUMAXB = 0x21c,
    /// Arm64 Operation LDUMAXH
    LDUMAXH = 0x21d,
    /// Arm64 Operation LDUMAXL
    LDUMAXL = 0x21e,
    /// Arm64 Operation LDUMAXLB
    LDUMAXLB = 0x21f,
    /// Arm64 Operation LDUMAXLH
    LDUMAXLH = 0x220,
    /// Arm64 Operation LDUMIN
    LDUMIN = 0x221,
    /// Arm64 Operation LDUMINA
    LDUMINA = 0x222,
    /// Arm64 Operation LDUMINAB
    LDUMINAB = 0x223,
    /// Arm64 Operation LDUMINAH
    LDUMINAH = 0x224,
    /// Arm64 Operation LDUMINAL
    LDUMINAL = 0x225,
    /// Arm64 Operation LDUMINALB
    LDUMINALB = 0x226,
    /// Arm64 Operation LDUMINALH
    LDUMINALH = 0x227,
    /// Arm64 Operation LDUMINB
    LDUMINB = 0x228,
    /// Arm64 Operation LDUMINH
    LDUMINH = 0x229,
    /// Arm64 Operation LDUMINL
    LDUMINL = 0x22a,
    /// Arm64 Operation LDUMINLB
    LDUMINLB = 0x22b,
    /// Arm64 Operation LDUMINLH
    LDUMINLH = 0x22c,
    /// Arm64 Operation LDUR
    LDUR = 0x22d,
    /// Arm64 Operation LDURB
    LDURB = 0x22e,
    /// Arm64 Operation LDURH
    LDURH = 0x22f,
    /// Arm64 Operation LDURSB
    LDURSB = 0x230,
    /// Arm64 Operation LDURSH
    LDURSH = 0x231,
    /// Arm64 Operation LDURSW
    LDURSW = 0x232,
    /// Arm64 Operation LDXP
    LDXP = 0x233,
    /// Arm64 Operation LDXR
    LDXR = 0x234,
    /// Arm64 Operation LDXRB
    LDXRB = 0x235,
    /// Arm64 Operation LDXRH
    LDXRH = 0x236,
    /// Arm64 Operation LSL
    LSL = 0x237,
    /// Arm64 Operation LSLR
    LSLR = 0x238,
    /// Arm64 Operation LSLV
    LSLV = 0x239,
    /// Arm64 Operation LSR
    LSR = 0x23a,
    /// Arm64 Operation LSRR
    LSRR = 0x23b,
    /// Arm64 Operation LSRV
    LSRV = 0x23c,
    /// Arm64 Operation MAD
    MAD = 0x23d,
    /// Arm64 Operation MADD
    MADD = 0x23e,
    /// Arm64 Operation MATCH
    MATCH = 0x23f,
    /// Arm64 Operation MLA
    MLA = 0x240,
    /// Arm64 Operation MLS
    MLS = 0x241,
    /// Arm64 Operation MNEG
    MNEG = 0x242,
    /// Arm64 Operation MOV
    MOV = 0x243,
    /// Arm64 Operation MOVA
    MOVA = 0x244,
    /// Arm64 Operation MOVI
    MOVI = 0x245,
    /// Arm64 Operation MOVK
    MOVK = 0x246,
    /// Arm64 Operation MOVN
    MOVN = 0x247,
    /// Arm64 Operation MOVPRFX
    MOVPRFX = 0x248,
    /// Arm64 Operation MOVS
    MOVS = 0x249,
    /// Arm64 Operation MOVZ
    MOVZ = 0x24a,
    /// Arm64 Operation MRS
    MRS = 0x24b,
    /// Arm64 Operation MSB
    MSB = 0x24c,
    /// Arm64 Operation MSR
    MSR = 0x24d,
    /// Arm64 Operation MSUB
    MSUB = 0x24e,
    /// Arm64 Operation MUL
    MUL = 0x24f,
    /// Arm64 Operation MVN
    MVN = 0x250,
    /// Arm64 Operation MVNI
    MVNI = 0x251,
    /// Arm64 Operation NAND
    NAND = 0x252,
    /// Arm64 Operation NANDS
    NANDS = 0x253,
    /// Arm64 Operation NBSL
    NBSL = 0x254,
    /// Arm64 Operation NEG
    NEG = 0x255,
    /// Arm64 Operation NEGS
    NEGS = 0x256,
    /// Arm64 Operation NGC
    NGC = 0x257,
    /// Arm64 Operation NGCS
    NGCS = 0x258,
    /// Arm64 Operation NMATCH
    NMATCH = 0x259,
    /// Arm64 Operation NOP
    NOP = 0x25a,
    /// Arm64 Operation NOR
    NOR = 0x25b,
    /// Arm64 Operation NORS
    NORS = 0x25c,
    /// Arm64 Operation NOT
    NOT = 0x25d,
    /// Arm64 Operation NOTS
    NOTS = 0x25e,
    /// Arm64 Operation ORN
    ORN = 0x25f,
    /// Arm64 Operation ORNS
    ORNS = 0x260,
    /// Arm64 Operation ORR
    ORR = 0x261,
    /// Arm64 Operation ORRS
    ORRS = 0x262,
    /// Arm64 Operation ORV
    ORV = 0x263,
    /// Arm64 Operation PACDA
    PACDA = 0x264,
    /// Arm64 Operation PACDB
    PACDB = 0x265,
    /// Arm64 Operation PACDZA
    PACDZA = 0x266,
    /// Arm64 Operation PACDZB
    PACDZB = 0x267,
    /// Arm64 Operation PACGA
    PACGA = 0x268,
    /// Arm64 Operation PACIA
    PACIA = 0x269,
    /// Arm64 Operation PACIA1716
    PACIA1716 = 0x26a,
    /// Arm64 Operation PACIASP
    PACIASP = 0x26b,
    /// Arm64 Operation PACIAZ
    PACIAZ = 0x26c,
    /// Arm64 Operation PACIB
    PACIB = 0x26d,
    /// Arm64 Operation PACIB1716
    PACIB1716 = 0x26e,
    /// Arm64 Operation PACIBSP
    PACIBSP = 0x26f,
    /// Arm64 Operation PACIBZ
    PACIBZ = 0x270,
    /// Arm64 Operation PACIZA
    PACIZA = 0x271,
    /// Arm64 Operation PACIZB
    PACIZB = 0x272,
    /// Arm64 Operation PFALSE
    PFALSE = 0x273,
    /// Arm64 Operation PFIRST
    PFIRST = 0x274,
    /// Arm64 Operation PMUL
    PMUL = 0x275,
    /// Arm64 Operation PMULL
    PMULL = 0x276,
    /// Arm64 Operation PMULL2
    PMULL2 = 0x277,
    /// Arm64 Operation PMULLB
    PMULLB = 0x278,
    /// Arm64 Operation PMULLT
    PMULLT = 0x279,
    /// Arm64 Operation PNEXT
    PNEXT = 0x27a,
    /// Arm64 Operation PRFB
    PRFB = 0x27b,
    /// Arm64 Operation PRFD
    PRFD = 0x27c,
    /// Arm64 Operation PRFH
    PRFH = 0x27d,
    /// Arm64 Operation PRFM
    PRFM = 0x27e,
    /// Arm64 Operation PRFUM
    PRFUM = 0x27f,
    /// Arm64 Operation PRFW
    PRFW = 0x280,
    /// Arm64 Operation PSB
    PSB = 0x281,
    /// Arm64 Operation PSSBB
    PSSBB = 0x282,
    /// Arm64 Operation PTEST
    PTEST = 0x283,
    /// Arm64 Operation PTRUE
    PTRUE = 0x284,
    /// Arm64 Operation PTRUES
    PTRUES = 0x285,
    /// Arm64 Operation PUNPKHI
    PUNPKHI = 0x286,
    /// Arm64 Operation PUNPKLO
    PUNPKLO = 0x287,
    /// Arm64 Operation RADDHN
    RADDHN = 0x288,
    /// Arm64 Operation RADDHN2
    RADDHN2 = 0x289,
    /// Arm64 Operation RADDHNB
    RADDHNB = 0x28a,
    /// Arm64 Operation RADDHNT
    RADDHNT = 0x28b,
    /// Arm64 Operation RAX1
    RAX1 = 0x28c,
    /// Arm64 Operation RBIT
    RBIT = 0x28d,
    /// Arm64 Operation RDFFR
    RDFFR = 0x28e,
    /// Arm64 Operation RDFFRS
    RDFFRS = 0x28f,
    /// Arm64 Operation RDVL
    RDVL = 0x290,
    /// Arm64 Operation RET
    RET = 0x291,
    /// Arm64 Operation RETAA
    RETAA = 0x292,
    /// Arm64 Operation RETAB
    RETAB = 0x293,
    /// Arm64 Operation REV
    REV = 0x294,
    /// Arm64 Operation REV16
    REV16 = 0x295,
    /// Arm64 Operation REV32
    REV32 = 0x296,
    /// Arm64 Operation REV64
    REV64 = 0x297,
    /// Arm64 Operation REVB
    REVB = 0x298,
    /// Arm64 Operation REVD
    REVD = 0x299,
    /// Arm64 Operation REVH
    REVH = 0x29a,
    /// Arm64 Operation REVW
    REVW = 0x29b,
    /// Arm64 Operation RMIF
    RMIF = 0x29c,
    /// Arm64 Operation ROR
    ROR = 0x29d,
    /// Arm64 Operation RORV
    RORV = 0x29e,
    /// Arm64 Operation RSHRN
    RSHRN = 0x29f,
    /// Arm64 Operation RSHRN2
    RSHRN2 = 0x2a0,
    /// Arm64 Operation RSHRNB
    RSHRNB = 0x2a1,
    /// Arm64 Operation RSHRNT
    RSHRNT = 0x2a2,
    /// Arm64 Operation RSUBHN
    RSUBHN = 0x2a3,
    /// Arm64 Operation RSUBHN2
    RSUBHN2 = 0x2a4,
    /// Arm64 Operation RSUBHNB
    RSUBHNB = 0x2a5,
    /// Arm64 Operation RSUBHNT
    RSUBHNT = 0x2a6,
    /// Arm64 Operation SABA
    SABA = 0x2a7,
    /// Arm64 Operation SABAL
    SABAL = 0x2a8,
    /// Arm64 Operation SABAL2
    SABAL2 = 0x2a9,
    /// Arm64 Operation SABALB
    SABALB = 0x2aa,
    /// Arm64 Operation SABALT
    SABALT = 0x2ab,
    /// Arm64 Operation SABD
    SABD = 0x2ac,
    /// Arm64 Operation SABDL
    SABDL = 0x2ad,
    /// Arm64 Operation SABDL2
    SABDL2 = 0x2ae,
    /// Arm64 Operation SABDLB
    SABDLB = 0x2af,
    /// Arm64 Operation SABDLT
    SABDLT = 0x2b0,
    /// Arm64 Operation SADALP
    SADALP = 0x2b1,
    /// Arm64 Operation SADDL
    SADDL = 0x2b2,
    /// Arm64 Operation SADDL2
    SADDL2 = 0x2b3,
    /// Arm64 Operation SADDLB
    SADDLB = 0x2b4,
    /// Arm64 Operation SADDLBT
    SADDLBT = 0x2b5,
    /// Arm64 Operation SADDLP
    SADDLP = 0x2b6,
    /// Arm64 Operation SADDLT
    SADDLT = 0x2b7,
    /// Arm64 Operation SADDLV
    SADDLV = 0x2b8,
    /// Arm64 Operation SADDV
    SADDV = 0x2b9,
    /// Arm64 Operation SADDW
    SADDW = 0x2ba,
    /// Arm64 Operation SADDW2
    SADDW2 = 0x2bb,
    /// Arm64 Operation SADDWB
    SADDWB = 0x2bc,
    /// Arm64 Operation SADDWT
    SADDWT = 0x2bd,
    /// Arm64 Operation SB
    SB = 0x2be,
    /// Arm64 Operation SBC
    SBC = 0x2bf,
    /// Arm64 Operation SBCLB
    SBCLB = 0x2c0,
    /// Arm64 Operation SBCLT
    SBCLT = 0x2c1,
    /// Arm64 Operation SBCS
    SBCS = 0x2c2,
    /// Arm64 Operation SBFIZ
    SBFIZ = 0x2c3,
    /// Arm64 Operation SBFM
    SBFM = 0x2c4,
    /// Arm64 Operation SBFX
    SBFX = 0x2c5,
    /// Arm64 Operation SCLAMP
    SCLAMP = 0x2c6,
    /// Arm64 Operation SCVTF
    SCVTF = 0x2c7,
    /// Arm64 Operation SDIV
    SDIV = 0x2c8,
    /// Arm64 Operation SDIVR
    SDIVR = 0x2c9,
    /// Arm64 Operation SDOT
    SDOT = 0x2ca,
    /// Arm64 Operation SEL
    SEL = 0x2cb,
    /// Arm64 Operation SETF16
    SETF16 = 0x2cc,
    /// Arm64 Operation SETF8
    SETF8 = 0x2cd,
    /// Arm64 Operation SETFFR
    SETFFR = 0x2ce,
    /// Arm64 Operation SEV
    SEV = 0x2cf,
    /// Arm64 Operation SEVL
    SEVL = 0x2d0,
    /// Arm64 Operation SHA1C
    SHA1C = 0x2d1,
    /// Arm64 Operation SHA1H
    SHA1H = 0x2d2,
    /// Arm64 Operation SHA1M
    SHA1M = 0x2d3,
    /// Arm64 Operation SHA1P
    SHA1P = 0x2d4,
    /// Arm64 Operation SHA1SU0
    SHA1SU0 = 0x2d5,
    /// Arm64 Operation SHA1SU1
    SHA1SU1 = 0x2d6,
    /// Arm64 Operation SHA256H
    SHA256H = 0x2d7,
    /// Arm64 Operation SHA256H2
    SHA256H2 = 0x2d8,
    /// Arm64 Operation SHA256SU0
    SHA256SU0 = 0x2d9,
    /// Arm64 Operation SHA256SU1
    SHA256SU1 = 0x2da,
    /// Arm64 Operation SHA512H
    SHA512H = 0x2db,
    /// Arm64 Operation SHA512H2
    SHA512H2 = 0x2dc,
    /// Arm64 Operation SHA512SU0
    SHA512SU0 = 0x2dd,
    /// Arm64 Operation SHA512SU1
    SHA512SU1 = 0x2de,
    /// Arm64 Operation SHADD
    SHADD = 0x2df,
    /// Arm64 Operation SHL
    SHL = 0x2e0,
    /// Arm64 Operation SHLL
    SHLL = 0x2e1,
    /// Arm64 Operation SHLL2
    SHLL2 = 0x2e2,
    /// Arm64 Operation SHRN
    SHRN = 0x2e3,
    /// Arm64 Operation SHRN2
    SHRN2 = 0x2e4,
    /// Arm64 Operation SHRNB
    SHRNB = 0x2e5,
    /// Arm64 Operation SHRNT
    SHRNT = 0x2e6,
    /// Arm64 Operation SHSUB
    SHSUB = 0x2e7,
    /// Arm64 Operation SHSUBR
    SHSUBR = 0x2e8,
    /// Arm64 Operation SLI
    SLI = 0x2e9,
    /// Arm64 Operation SM3PARTW1
    SM3PARTW1 = 0x2ea,
    /// Arm64 Operation SM3PARTW2
    SM3PARTW2 = 0x2eb,
    /// Arm64 Operation SM3SS1
    SM3SS1 = 0x2ec,
    /// Arm64 Operation SM3TT1A
    SM3TT1A = 0x2ed,
    /// Arm64 Operation SM3TT1B
    SM3TT1B = 0x2ee,
    /// Arm64 Operation SM3TT2A
    SM3TT2A = 0x2ef,
    /// Arm64 Operation SM3TT2B
    SM3TT2B = 0x2f0,
    /// Arm64 Operation SM4E
    SM4E = 0x2f1,
    /// Arm64 Operation SM4EKEY
    SM4EKEY = 0x2f2,
    /// Arm64 Operation SMADDL
    SMADDL = 0x2f3,
    /// Arm64 Operation SMAX
    SMAX = 0x2f4,
    /// Arm64 Operation SMAXP
    SMAXP = 0x2f5,
    /// Arm64 Operation SMAXV
    SMAXV = 0x2f6,
    /// Arm64 Operation SMC
    SMC = 0x2f7,
    /// Arm64 Operation SMIN
    SMIN = 0x2f8,
    /// Arm64 Operation SMINP
    SMINP = 0x2f9,
    /// Arm64 Operation SMINV
    SMINV = 0x2fa,
    /// Arm64 Operation SMLAL
    SMLAL = 0x2fb,
    /// Arm64 Operation SMLAL2
    SMLAL2 = 0x2fc,
    /// Arm64 Operation SMLALB
    SMLALB = 0x2fd,
    /// Arm64 Operation SMLALT
    SMLALT = 0x2fe,
    /// Arm64 Operation SMLSL
    SMLSL = 0x2ff,
    /// Arm64 Operation SMLSL2
    SMLSL2 = 0x300,
    /// Arm64 Operation SMLSLB
    SMLSLB = 0x301,
    /// Arm64 Operation SMLSLT
    SMLSLT = 0x302,
    /// Arm64 Operation SMMLA
    SMMLA = 0x303,
    /// Arm64 Operation SMNEGL
    SMNEGL = 0x304,
    /// Arm64 Operation SMOPA
    SMOPA = 0x305,
    /// Arm64 Operation SMOPS
    SMOPS = 0x306,
    /// Arm64 Operation SMOV
    SMOV = 0x307,
    /// Arm64 Operation SMSTART
    SMSTART = 0x308,
    /// Arm64 Operation SMSTOP
    SMSTOP = 0x309,
    /// Arm64 Operation SMSUBL
    SMSUBL = 0x30a,
    /// Arm64 Operation SMULH
    SMULH = 0x30b,
    /// Arm64 Operation SMULL
    SMULL = 0x30c,
    /// Arm64 Operation SMULL2
    SMULL2 = 0x30d,
    /// Arm64 Operation SMULLB
    SMULLB = 0x30e,
    /// Arm64 Operation SMULLT
    SMULLT = 0x30f,
    /// Arm64 Operation SPLICE
    SPLICE = 0x310,
    /// Arm64 Operation SQABS
    SQABS = 0x311,
    /// Arm64 Operation SQADD
    SQADD = 0x312,
    /// Arm64 Operation SQCADD
    SQCADD = 0x313,
    /// Arm64 Operation SQDECB
    SQDECB = 0x314,
    /// Arm64 Operation SQDECD
    SQDECD = 0x315,
    /// Arm64 Operation SQDECH
    SQDECH = 0x316,
    /// Arm64 Operation SQDECP
    SQDECP = 0x317,
    /// Arm64 Operation SQDECW
    SQDECW = 0x318,
    /// Arm64 Operation SQDMLAL
    SQDMLAL = 0x319,
    /// Arm64 Operation SQDMLAL2
    SQDMLAL2 = 0x31a,
    /// Arm64 Operation SQDMLALB
    SQDMLALB = 0x31b,
    /// Arm64 Operation SQDMLALBT
    SQDMLALBT = 0x31c,
    /// Arm64 Operation SQDMLALT
    SQDMLALT = 0x31d,
    /// Arm64 Operation SQDMLSL
    SQDMLSL = 0x31e,
    /// Arm64 Operation SQDMLSL2
    SQDMLSL2 = 0x31f,
    /// Arm64 Operation SQDMLSLB
    SQDMLSLB = 0x320,
    /// Arm64 Operation SQDMLSLBT
    SQDMLSLBT = 0x321,
    /// Arm64 Operation SQDMLSLT
    SQDMLSLT = 0x322,
    /// Arm64 Operation SQDMULH
    SQDMULH = 0x323,
    /// Arm64 Operation SQDMULL
    SQDMULL = 0x324,
    /// Arm64 Operation SQDMULL2
    SQDMULL2 = 0x325,
    /// Arm64 Operation SQDMULLB
    SQDMULLB = 0x326,
    /// Arm64 Operation SQDMULLT
    SQDMULLT = 0x327,
    /// Arm64 Operation SQINCB
    SQINCB = 0x328,
    /// Arm64 Operation SQINCD
    SQINCD = 0x329,
    /// Arm64 Operation SQINCH
    SQINCH = 0x32a,
    /// Arm64 Operation SQINCP
    SQINCP = 0x32b,
    /// Arm64 Operation SQINCW
    SQINCW = 0x32c,
    /// Arm64 Operation SQNEG
    SQNEG = 0x32d,
    /// Arm64 Operation SQRDCMLAH
    SQRDCMLAH = 0x32e,
    /// Arm64 Operation SQRDMLAH
    SQRDMLAH = 0x32f,
    /// Arm64 Operation SQRDMLSH
    SQRDMLSH = 0x330,
    /// Arm64 Operation SQRDMULH
    SQRDMULH = 0x331,
    /// Arm64 Operation SQRSHL
    SQRSHL = 0x332,
    /// Arm64 Operation SQRSHLR
    SQRSHLR = 0x333,
    /// Arm64 Operation SQRSHRN
    SQRSHRN = 0x334,
    /// Arm64 Operation SQRSHRN2
    SQRSHRN2 = 0x335,
    /// Arm64 Operation SQRSHRNB
    SQRSHRNB = 0x336,
    /// Arm64 Operation SQRSHRNT
    SQRSHRNT = 0x337,
    /// Arm64 Operation SQRSHRUN
    SQRSHRUN = 0x338,
    /// Arm64 Operation SQRSHRUN2
    SQRSHRUN2 = 0x339,
    /// Arm64 Operation SQRSHRUNB
    SQRSHRUNB = 0x33a,
    /// Arm64 Operation SQRSHRUNT
    SQRSHRUNT = 0x33b,
    /// Arm64 Operation SQSHL
    SQSHL = 0x33c,
    /// Arm64 Operation SQSHLR
    SQSHLR = 0x33d,
    /// Arm64 Operation SQSHLU
    SQSHLU = 0x33e,
    /// Arm64 Operation SQSHRN
    SQSHRN = 0x33f,
    /// Arm64 Operation SQSHRN2
    SQSHRN2 = 0x340,
    /// Arm64 Operation SQSHRNB
    SQSHRNB = 0x341,
    /// Arm64 Operation SQSHRNT
    SQSHRNT = 0x342,
    /// Arm64 Operation SQSHRUN
    SQSHRUN = 0x343,
    /// Arm64 Operation SQSHRUN2
    SQSHRUN2 = 0x344,
    /// Arm64 Operation SQSHRUNB
    SQSHRUNB = 0x345,
    /// Arm64 Operation SQSHRUNT
    SQSHRUNT = 0x346,
    /// Arm64 Operation SQSUB
    SQSUB = 0x347,
    /// Arm64 Operation SQSUBR
    SQSUBR = 0x348,
    /// Arm64 Operation SQXTN
    SQXTN = 0x349,
    /// Arm64 Operation SQXTN2
    SQXTN2 = 0x34a,
    /// Arm64 Operation SQXTNB
    SQXTNB = 0x34b,
    /// Arm64 Operation SQXTNT
    SQXTNT = 0x34c,
    /// Arm64 Operation SQXTUN
    SQXTUN = 0x34d,
    /// Arm64 Operation SQXTUN2
    SQXTUN2 = 0x34e,
    /// Arm64 Operation SQXTUNB
    SQXTUNB = 0x34f,
    /// Arm64 Operation SQXTUNT
    SQXTUNT = 0x350,
    /// Arm64 Operation SRHADD
    SRHADD = 0x351,
    /// Arm64 Operation SRI
    SRI = 0x352,
    /// Arm64 Operation SRSHL
    SRSHL = 0x353,
    /// Arm64 Operation SRSHLR
    SRSHLR = 0x354,
    /// Arm64 Operation SRSHR
    SRSHR = 0x355,
    /// Arm64 Operation SRSRA
    SRSRA = 0x356,
    /// Arm64 Operation SSBB
    SSBB = 0x357,
    /// Arm64 Operation SSHL
    SSHL = 0x358,
    /// Arm64 Operation SSHLL
    SSHLL = 0x359,
    /// Arm64 Operation SSHLL2
    SSHLL2 = 0x35a,
    /// Arm64 Operation SSHLLB
    SSHLLB = 0x35b,
    /// Arm64 Operation SSHLLT
    SSHLLT = 0x35c,
    /// Arm64 Operation SSHR
    SSHR = 0x35d,
    /// Arm64 Operation SSRA
    SSRA = 0x35e,
    /// Arm64 Operation SSUBL
    SSUBL = 0x35f,
    /// Arm64 Operation SSUBL2
    SSUBL2 = 0x360,
    /// Arm64 Operation SSUBLB
    SSUBLB = 0x361,
    /// Arm64 Operation SSUBLBT
    SSUBLBT = 0x362,
    /// Arm64 Operation SSUBLT
    SSUBLT = 0x363,
    /// Arm64 Operation SSUBLTB
    SSUBLTB = 0x364,
    /// Arm64 Operation SSUBW
    SSUBW = 0x365,
    /// Arm64 Operation SSUBW2
    SSUBW2 = 0x366,
    /// Arm64 Operation SSUBWB
    SSUBWB = 0x367,
    /// Arm64 Operation SSUBWT
    SSUBWT = 0x368,
    /// Arm64 Operation ST1
    ST1 = 0x369,
    /// Arm64 Operation ST1B
    ST1B = 0x36a,
    /// Arm64 Operation ST1D
    ST1D = 0x36b,
    /// Arm64 Operation ST1H
    ST1H = 0x36c,
    /// Arm64 Operation ST1Q
    ST1Q = 0x36d,
    /// Arm64 Operation ST1W
    ST1W = 0x36e,
    /// Arm64 Operation ST2
    ST2 = 0x36f,
    /// Arm64 Operation ST2B
    ST2B = 0x370,
    /// Arm64 Operation ST2D
    ST2D = 0x371,
    /// Arm64 Operation ST2G
    ST2G = 0x372,
    /// Arm64 Operation ST2H
    ST2H = 0x373,
    /// Arm64 Operation ST2W
    ST2W = 0x374,
    /// Arm64 Operation ST3
    ST3 = 0x375,
    /// Arm64 Operation ST3B
    ST3B = 0x376,
    /// Arm64 Operation ST3D
    ST3D = 0x377,
    /// Arm64 Operation ST3H
    ST3H = 0x378,
    /// Arm64 Operation ST3W
    ST3W = 0x379,
    /// Arm64 Operation ST4
    ST4 = 0x37a,
    /// Arm64 Operation ST4B
    ST4B = 0x37b,
    /// Arm64 Operation ST4D
    ST4D = 0x37c,
    /// Arm64 Operation ST4H
    ST4H = 0x37d,
    /// Arm64 Operation ST4W
    ST4W = 0x37e,
    /// Arm64 Operation ST64B
    ST64B = 0x37f,
    /// Arm64 Operation ST64BV
    ST64BV = 0x380,
    /// Arm64 Operation ST64BV0
    ST64BV0 = 0x381,
    /// Arm64 Operation STADD
    STADD = 0x382,
    /// Arm64 Operation STADDB
    STADDB = 0x383,
    /// Arm64 Operation STADDH
    STADDH = 0x384,
    /// Arm64 Operation STADDL
    STADDL = 0x385,
    /// Arm64 Operation STADDLB
    STADDLB = 0x386,
    /// Arm64 Operation STADDLH
    STADDLH = 0x387,
    /// Arm64 Operation STCLR
    STCLR = 0x388,
    /// Arm64 Operation STCLRB
    STCLRB = 0x389,
    /// Arm64 Operation STCLRH
    STCLRH = 0x38a,
    /// Arm64 Operation STCLRL
    STCLRL = 0x38b,
    /// Arm64 Operation STCLRLB
    STCLRLB = 0x38c,
    /// Arm64 Operation STCLRLH
    STCLRLH = 0x38d,
    /// Arm64 Operation STEOR
    STEOR = 0x38e,
    /// Arm64 Operation STEORB
    STEORB = 0x38f,
    /// Arm64 Operation STEORH
    STEORH = 0x390,
    /// Arm64 Operation STEORL
    STEORL = 0x391,
    /// Arm64 Operation STEORLB
    STEORLB = 0x392,
    /// Arm64 Operation STEORLH
    STEORLH = 0x393,
    /// Arm64 Operation STG
    STG = 0x394,
    /// Arm64 Operation STGM
    STGM = 0x395,
    /// Arm64 Operation STGP
    STGP = 0x396,
    /// Arm64 Operation STLLR
    STLLR = 0x397,
    /// Arm64 Operation STLLRB
    STLLRB = 0x398,
    /// Arm64 Operation STLLRH
    STLLRH = 0x399,
    /// Arm64 Operation STLR
    STLR = 0x39a,
    /// Arm64 Operation STLRB
    STLRB = 0x39b,
    /// Arm64 Operation STLRH
    STLRH = 0x39c,
    /// Arm64 Operation STLUR
    STLUR = 0x39d,
    /// Arm64 Operation STLURB
    STLURB = 0x39e,
    /// Arm64 Operation STLURH
    STLURH = 0x39f,
    /// Arm64 Operation STLXP
    STLXP = 0x3a0,
    /// Arm64 Operation STLXR
    STLXR = 0x3a1,
    /// Arm64 Operation STLXRB
    STLXRB = 0x3a2,
    /// Arm64 Operation STLXRH
    STLXRH = 0x3a3,
    /// Arm64 Operation STNP
    STNP = 0x3a4,
    /// Arm64 Operation STNT1B
    STNT1B = 0x3a5,
    /// Arm64 Operation STNT1D
    STNT1D = 0x3a6,
    /// Arm64 Operation STNT1H
    STNT1H = 0x3a7,
    /// Arm64 Operation STNT1W
    STNT1W = 0x3a8,
    /// Arm64 Operation STP
    STP = 0x3a9,
    /// Arm64 Operation STR
    STR = 0x3aa,
    /// Arm64 Operation STRB
    STRB = 0x3ab,
    /// Arm64 Operation STRH
    STRH = 0x3ac,
    /// Arm64 Operation STSET
    STSET = 0x3ad,
    /// Arm64 Operation STSETB
    STSETB = 0x3ae,
    /// Arm64 Operation STSETH
    STSETH = 0x3af,
    /// Arm64 Operation STSETL
    STSETL = 0x3b0,
    /// Arm64 Operation STSETLB
    STSETLB = 0x3b1,
    /// Arm64 Operation STSETLH
    STSETLH = 0x3b2,
    /// Arm64 Operation STSMAX
    STSMAX = 0x3b3,
    /// Arm64 Operation STSMAXB
    STSMAXB = 0x3b4,
    /// Arm64 Operation STSMAXH
    STSMAXH = 0x3b5,
    /// Arm64 Operation STSMAXL
    STSMAXL = 0x3b6,
    /// Arm64 Operation STSMAXLB
    STSMAXLB = 0x3b7,
    /// Arm64 Operation STSMAXLH
    STSMAXLH = 0x3b8,
    /// Arm64 Operation STSMIN
    STSMIN = 0x3b9,
    /// Arm64 Operation STSMINB
    STSMINB = 0x3ba,
    /// Arm64 Operation STSMINH
    STSMINH = 0x3bb,
    /// Arm64 Operation STSMINL
    STSMINL = 0x3bc,
    /// Arm64 Operation STSMINLB
    STSMINLB = 0x3bd,
    /// Arm64 Operation STSMINLH
    STSMINLH = 0x3be,
    /// Arm64 Operation STTR
    STTR = 0x3bf,
    /// Arm64 Operation STTRB
    STTRB = 0x3c0,
    /// Arm64 Operation STTRH
    STTRH = 0x3c1,
    /// Arm64 Operation STUMAX
    STUMAX = 0x3c2,
    /// Arm64 Operation STUMAXB
    STUMAXB = 0x3c3,
    /// Arm64 Operation STUMAXH
    STUMAXH = 0x3c4,
    /// Arm64 Operation STUMAXL
    STUMAXL = 0x3c5,
    /// Arm64 Operation STUMAXLB
    STUMAXLB = 0x3c6,
    /// Arm64 Operation STUMAXLH
    STUMAXLH = 0x3c7,
    /// Arm64 Operation STUMIN
    STUMIN = 0x3c8,
    /// Arm64 Operation STUMINB
    STUMINB = 0x3c9,
    /// Arm64 Operation STUMINH
    STUMINH = 0x3ca,
    /// Arm64 Operation STUMINL
    STUMINL = 0x3cb,
    /// Arm64 Operation STUMINLB
    STUMINLB = 0x3cc,
    /// Arm64 Operation STUMINLH
    STUMINLH = 0x3cd,
    /// Arm64 Operation STUR
    STUR = 0x3ce,
    /// Arm64 Operation STURB
    STURB = 0x3cf,
    /// Arm64 Operation STURH
    STURH = 0x3d0,
    /// Arm64 Operation STXP
    STXP = 0x3d1,
    /// Arm64 Operation STXR
    STXR = 0x3d2,
    /// Arm64 Operation STXRB
    STXRB = 0x3d3,
    /// Arm64 Operation STXRH
    STXRH = 0x3d4,
    /// Arm64 Operation STZ2G
    STZ2G = 0x3d5,
    /// Arm64 Operation STZG
    STZG = 0x3d6,
    /// Arm64 Operation STZGM
    STZGM = 0x3d7,
    /// Arm64 Operation SUB
    SUB = 0x3d8,
    /// Arm64 Operation SUBG
    SUBG = 0x3d9,
    /// Arm64 Operation SUBHN
    SUBHN = 0x3da,
    /// Arm64 Operation SUBHN2
    SUBHN2 = 0x3db,
    /// Arm64 Operation SUBHNB
    SUBHNB = 0x3dc,
    /// Arm64 Operation SUBHNT
    SUBHNT = 0x3dd,
    /// Arm64 Operation SUBP
    SUBP = 0x3de,
    /// Arm64 Operation SUBPS
    SUBPS = 0x3df,
    /// Arm64 Operation SUBR
    SUBR = 0x3e0,
    /// Arm64 Operation SUBS
    SUBS = 0x3e1,
    /// Arm64 Operation SUDOT
    SUDOT = 0x3e2,
    /// Arm64 Operation SUMOPA
    SUMOPA = 0x3e3,
    /// Arm64 Operation SUMOPS
    SUMOPS = 0x3e4,
    /// Arm64 Operation SUNPKHI
    SUNPKHI = 0x3e5,
    /// Arm64 Operation SUNPKLO
    SUNPKLO = 0x3e6,
    /// Arm64 Operation SUQADD
    SUQADD = 0x3e7,
    /// Arm64 Operation SVC
    SVC = 0x3e8,
    /// Arm64 Operation SWP
    SWP = 0x3e9,
    /// Arm64 Operation SWPA
    SWPA = 0x3ea,
    /// Arm64 Operation SWPAB
    SWPAB = 0x3eb,
    /// Arm64 Operation SWPAH
    SWPAH = 0x3ec,
    /// Arm64 Operation SWPAL
    SWPAL = 0x3ed,
    /// Arm64 Operation SWPALB
    SWPALB = 0x3ee,
    /// Arm64 Operation SWPALH
    SWPALH = 0x3ef,
    /// Arm64 Operation SWPB
    SWPB = 0x3f0,
    /// Arm64 Operation SWPH
    SWPH = 0x3f1,
    /// Arm64 Operation SWPL
    SWPL = 0x3f2,
    /// Arm64 Operation SWPLB
    SWPLB = 0x3f3,
    /// Arm64 Operation SWPLH
    SWPLH = 0x3f4,
    /// Arm64 Operation SXTB
    SXTB = 0x3f5,
    /// Arm64 Operation SXTH
    SXTH = 0x3f6,
    /// Arm64 Operation SXTL
    SXTL = 0x3f7,
    /// Arm64 Operation SXTL2
    SXTL2 = 0x3f8,
    /// Arm64 Operation SXTW
    SXTW = 0x3f9,
    /// Arm64 Operation SYS
    SYS = 0x3fa,
    /// Arm64 Operation SYSL
    SYSL = 0x3fb,
    /// Arm64 Operation TBL
    TBL = 0x3fc,
    /// Arm64 Operation TBNZ
    TBNZ = 0x3fd,
    /// Arm64 Operation TBX
    TBX = 0x3fe,
    /// Arm64 Operation TBZ
    TBZ = 0x3ff,
    /// Arm64 Operation TCANCEL
    TCANCEL = 0x400,
    /// Arm64 Operation TCOMMIT
    TCOMMIT = 0x401,
    /// Arm64 Operation TLBI
    TLBI = 0x402,
    /// Arm64 Operation TRN1
    TRN1 = 0x403,
    /// Arm64 Operation TRN2
    TRN2 = 0x404,
    /// Arm64 Operation TSB
    TSB = 0x405,
    /// Arm64 Operation TST
    TST = 0x406,
    /// Arm64 Operation TSTART
    TSTART = 0x407,
    /// Arm64 Operation TTEST
    TTEST = 0x408,
    /// Arm64 Operation UABA
    UABA = 0x409,
    /// Arm64 Operation UABAL
    UABAL = 0x40a,
    /// Arm64 Operation UABAL2
    UABAL2 = 0x40b,
    /// Arm64 Operation UABALB
    UABALB = 0x40c,
    /// Arm64 Operation UABALT
    UABALT = 0x40d,
    /// Arm64 Operation UABD
    UABD = 0x40e,
    /// Arm64 Operation UABDL
    UABDL = 0x40f,
    /// Arm64 Operation UABDL2
    UABDL2 = 0x410,
    /// Arm64 Operation UABDLB
    UABDLB = 0x411,
    /// Arm64 Operation UABDLT
    UABDLT = 0x412,
    /// Arm64 Operation UADALP
    UADALP = 0x413,
    /// Arm64 Operation UADDL
    UADDL = 0x414,
    /// Arm64 Operation UADDL2
    UADDL2 = 0x415,
    /// Arm64 Operation UADDLB
    UADDLB = 0x416,
    /// Arm64 Operation UADDLP
    UADDLP = 0x417,
    /// Arm64 Operation UADDLT
    UADDLT = 0x418,
    /// Arm64 Operation UADDLV
    UADDLV = 0x419,
    /// Arm64 Operation UADDV
    UADDV = 0x41a,
    /// Arm64 Operation UADDW
    UADDW = 0x41b,
    /// Arm64 Operation UADDW2
    UADDW2 = 0x41c,
    /// Arm64 Operation UADDWB
    UADDWB = 0x41d,
    /// Arm64 Operation UADDWT
    UADDWT = 0x41e,
    /// Arm64 Operation UBFIZ
    UBFIZ = 0x41f,
    /// Arm64 Operation UBFM
    UBFM = 0x420,
    /// Arm64 Operation UBFX
    UBFX = 0x421,
    /// Arm64 Operation UCLAMP
    UCLAMP = 0x422,
    /// Arm64 Operation UCVTF
    UCVTF = 0x423,
    /// Arm64 Operation UDF
    UDF = 0x424,
    /// Arm64 Operation UDIV
    UDIV = 0x425,
    /// Arm64 Operation UDIVR
    UDIVR = 0x426,
    /// Arm64 Operation UDOT
    UDOT = 0x427,
    /// Arm64 Operation UHADD
    UHADD = 0x428,
    /// Arm64 Operation UHSUB
    UHSUB = 0x429,
    /// Arm64 Operation UHSUBR
    UHSUBR = 0x42a,
    /// Arm64 Operation UMADDL
    UMADDL = 0x42b,
    /// Arm64 Operation UMAX
    UMAX = 0x42c,
    /// Arm64 Operation UMAXP
    UMAXP = 0x42d,
    /// Arm64 Operation UMAXV
    UMAXV = 0x42e,
    /// Arm64 Operation UMIN
    UMIN = 0x42f,
    /// Arm64 Operation UMINP
    UMINP = 0x430,
    /// Arm64 Operation UMINV
    UMINV = 0x431,
    /// Arm64 Operation UMLAL
    UMLAL = 0x432,
    /// Arm64 Operation UMLAL2
    UMLAL2 = 0x433,
    /// Arm64 Operation UMLALB
    UMLALB = 0x434,
    /// Arm64 Operation UMLALT
    UMLALT = 0x435,
    /// Arm64 Operation UMLSL
    UMLSL = 0x436,
    /// Arm64 Operation UMLSL2
    UMLSL2 = 0x437,
    /// Arm64 Operation UMLSLB
    UMLSLB = 0x438,
    /// Arm64 Operation UMLSLT
    UMLSLT = 0x439,
    /// Arm64 Operation UMMLA
    UMMLA = 0x43a,
    /// Arm64 Operation UMNEGL
    UMNEGL = 0x43b,
    /// Arm64 Operation UMOPA
    UMOPA = 0x43c,
    /// Arm64 Operation UMOPS
    UMOPS = 0x43d,
    /// Arm64 Operation UMOV
    UMOV = 0x43e,
    /// Arm64 Operation UMSUBL
    UMSUBL = 0x43f,
    /// Arm64 Operation UMULH
    UMULH = 0x440,
    /// Arm64 Operation UMULL
    UMULL = 0x441,
    /// Arm64 Operation UMULL2
    UMULL2 = 0x442,
    /// Arm64 Operation UMULLB
    UMULLB = 0x443,
    /// Arm64 Operation UMULLT
    UMULLT = 0x444,
    /// Arm64 Operation UQADD
    UQADD = 0x445,
    /// Arm64 Operation UQDECB
    UQDECB = 0x446,
    /// Arm64 Operation UQDECD
    UQDECD = 0x447,
    /// Arm64 Operation UQDECH
    UQDECH = 0x448,
    /// Arm64 Operation UQDECP
    UQDECP = 0x449,
    /// Arm64 Operation UQDECW
    UQDECW = 0x44a,
    /// Arm64 Operation UQINCB
    UQINCB = 0x44b,
    /// Arm64 Operation UQINCD
    UQINCD = 0x44c,
    /// Arm64 Operation UQINCH
    UQINCH = 0x44d,
    /// Arm64 Operation UQINCP
    UQINCP = 0x44e,
    /// Arm64 Operation UQINCW
    UQINCW = 0x44f,
    /// Arm64 Operation UQRSHL
    UQRSHL = 0x450,
    /// Arm64 Operation UQRSHLR
    UQRSHLR = 0x451,
    /// Arm64 Operation UQRSHRN
    UQRSHRN = 0x452,
    /// Arm64 Operation UQRSHRN2
    UQRSHRN2 = 0x453,
    /// Arm64 Operation UQRSHRNB
    UQRSHRNB = 0x454,
    /// Arm64 Operation UQRSHRNT
    UQRSHRNT = 0x455,
    /// Arm64 Operation UQSHL
    UQSHL = 0x456,
    /// Arm64 Operation UQSHLR
    UQSHLR = 0x457,
    /// Arm64 Operation UQSHRN
    UQSHRN = 0x458,
    /// Arm64 Operation UQSHRN2
    UQSHRN2 = 0x459,
    /// Arm64 Operation UQSHRNB
    UQSHRNB = 0x45a,
    /// Arm64 Operation UQSHRNT
    UQSHRNT = 0x45b,
    /// Arm64 Operation UQSUB
    UQSUB = 0x45c,
    /// Arm64 Operation UQSUBR
    UQSUBR = 0x45d,
    /// Arm64 Operation UQXTN
    UQXTN = 0x45e,
    /// Arm64 Operation UQXTN2
    UQXTN2 = 0x45f,
    /// Arm64 Operation UQXTNB
    UQXTNB = 0x460,
    /// Arm64 Operation UQXTNT
    UQXTNT = 0x461,
    /// Arm64 Operation URECPE
    URECPE = 0x462,
    /// Arm64 Operation URHADD
    URHADD = 0x463,
    /// Arm64 Operation URSHL
    URSHL = 0x464,
    /// Arm64 Operation URSHLR
    URSHLR = 0x465,
    /// Arm64 Operation URSHR
    URSHR = 0x466,
    /// Arm64 Operation URSQRTE
    URSQRTE = 0x467,
    /// Arm64 Operation URSRA
    URSRA = 0x468,
    /// Arm64 Operation USDOT
    USDOT = 0x469,
    /// Arm64 Operation USHL
    USHL = 0x46a,
    /// Arm64 Operation USHLL
    USHLL = 0x46b,
    /// Arm64 Operation USHLL2
    USHLL2 = 0x46c,
    /// Arm64 Operation USHLLB
    USHLLB = 0x46d,
    /// Arm64 Operation USHLLT
    USHLLT = 0x46e,
    /// Arm64 Operation USHR
    USHR = 0x46f,
    /// Arm64 Operation USMMLA
    USMMLA = 0x470,
    /// Arm64 Operation USMOPA
    USMOPA = 0x471,
    /// Arm64 Operation USMOPS
    USMOPS = 0x472,
    /// Arm64 Operation USQADD
    USQADD = 0x473,
    /// Arm64 Operation USRA
    USRA = 0x474,
    /// Arm64 Operation USUBL
    USUBL = 0x475,
    /// Arm64 Operation USUBL2
    USUBL2 = 0x476,
    /// Arm64 Operation USUBLB
    USUBLB = 0x477,
    /// Arm64 Operation USUBLT
    USUBLT = 0x478,
    /// Arm64 Operation USUBW
    USUBW = 0x479,
    /// Arm64 Operation USUBW2
    USUBW2 = 0x47a,
    /// Arm64 Operation USUBWB
    USUBWB = 0x47b,
    /// Arm64 Operation USUBWT
    USUBWT = 0x47c,
    /// Arm64 Operation UUNPKHI
    UUNPKHI = 0x47d,
    /// Arm64 Operation UUNPKLO
    UUNPKLO = 0x47e,
    /// Arm64 Operation UXTB
    UXTB = 0x47f,
    /// Arm64 Operation UXTH
    UXTH = 0x480,
    /// Arm64 Operation UXTL
    UXTL = 0x481,
    /// Arm64 Operation UXTL2
    UXTL2 = 0x482,
    /// Arm64 Operation UXTW
    UXTW = 0x483,
    /// Arm64 Operation UZP1
    UZP1 = 0x484,
    /// Arm64 Operation UZP2
    UZP2 = 0x485,
    /// Arm64 Operation WFE
    WFE = 0x486,
    /// Arm64 Operation WFET
    WFET = 0x487,
    /// Arm64 Operation WFI
    WFI = 0x488,
    /// Arm64 Operation WFIT
    WFIT = 0x489,
    /// Arm64 Operation WHILEGE
    WHILEGE = 0x48a,
    /// Arm64 Operation WHILEGT
    WHILEGT = 0x48b,
    /// Arm64 Operation WHILEHI
    WHILEHI = 0x48c,
    /// Arm64 Operation WHILEHS
    WHILEHS = 0x48d,
    /// Arm64 Operation WHILELE
    WHILELE = 0x48e,
    /// Arm64 Operation WHILELO
    WHILELO = 0x48f,
    /// Arm64 Operation WHILELS
    WHILELS = 0x490,
    /// Arm64 Operation WHILELT
    WHILELT = 0x491,
    /// Arm64 Operation WHILERW
    WHILERW = 0x492,
    /// Arm64 Operation WHILEWR
    WHILEWR = 0x493,
    /// Arm64 Operation WRFFR
    WRFFR = 0x494,
    /// Arm64 Operation XAFLAG
    XAFLAG = 0x495,
    /// Arm64 Operation XAR
    XAR = 0x496,
    /// Arm64 Operation XPACD
    XPACD = 0x497,
    /// Arm64 Operation XPACI
    XPACI = 0x498,
    /// Arm64 Operation XPACLRI
    XPACLRI = 0x499,
    /// Arm64 Operation XTN
    XTN = 0x49a,
    /// Arm64 Operation XTN2
    XTN2 = 0x49b,
    /// Arm64 Operation YIELD
    YIELD = 0x49c,
    /// Arm64 Operation ZERO
    ZERO = 0x49d,
    /// Arm64 Operation ZIP1
    ZIP1 = 0x49e,
    /// Arm64 Operation ZIP2
    ZIP2 = 0x49f,
}

impl Display for OperationType {
    /// Print arm64 operation name
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        match self {
            OperationType::ERROR => write!(f, "ERROR"),
            OperationType::ABS => write!(f, "ABS"),
            OperationType::ADC => write!(f, "ADC"),
            OperationType::ADCLB => write!(f, "ADCLB"),
            OperationType::ADCLT => write!(f, "ADCLT"),
            OperationType::ADCS => write!(f, "ADCS"),
            OperationType::ADD => write!(f, "ADD"),
            OperationType::ADDG => write!(f, "ADDG"),
            OperationType::ADDHA => write!(f, "ADDHA"),
            OperationType::ADDHN => write!(f, "ADDHN"),
            OperationType::ADDHN2 => write!(f, "ADDHN2"),
            OperationType::ADDHNB => write!(f, "ADDHNB"),
            OperationType::ADDHNT => write!(f, "ADDHNT"),
            OperationType::ADDP => write!(f, "ADDP"),
            OperationType::ADDPL => write!(f, "ADDPL"),
            OperationType::ADDS => write!(f, "ADDS"),
            OperationType::ADDV => write!(f, "ADDV"),
            OperationType::ADDVA => write!(f, "ADDVA"),
            OperationType::ADDVL => write!(f, "ADDVL"),
            OperationType::ADR => write!(f, "ADR"),
            OperationType::ADRP => write!(f, "ADRP"),
            OperationType::AESD => write!(f, "AESD"),
            OperationType::AESE => write!(f, "AESE"),
            OperationType::AESIMC => write!(f, "AESIMC"),
            OperationType::AESMC => write!(f, "AESMC"),
            OperationType::AND => write!(f, "AND"),
            OperationType::ANDS => write!(f, "ANDS"),
            OperationType::ANDV => write!(f, "ANDV"),
            OperationType::ASR => write!(f, "ASR"),
            OperationType::ASRD => write!(f, "ASRD"),
            OperationType::ASRR => write!(f, "ASRR"),
            OperationType::ASRV => write!(f, "ASRV"),
            OperationType::AT => write!(f, "AT"),
            OperationType::AUTDA => write!(f, "AUTDA"),
            OperationType::AUTDB => write!(f, "AUTDB"),
            OperationType::AUTDZA => write!(f, "AUTDZA"),
            OperationType::AUTDZB => write!(f, "AUTDZB"),
            OperationType::AUTIA => write!(f, "AUTIA"),
            OperationType::AUTIA1716 => write!(f, "AUTIA1716"),
            OperationType::AUTIASP => write!(f, "AUTIASP"),
            OperationType::AUTIAZ => write!(f, "AUTIAZ"),
            OperationType::AUTIB => write!(f, "AUTIB"),
            OperationType::AUTIB1716 => write!(f, "AUTIB1716"),
            OperationType::AUTIBSP => write!(f, "AUTIBSP"),
            OperationType::AUTIBZ => write!(f, "AUTIBZ"),
            OperationType::AUTIZA => write!(f, "AUTIZA"),
            OperationType::AUTIZB => write!(f, "AUTIZB"),
            OperationType::AXFLAG => write!(f, "AXFLAG"),
            OperationType::B => write!(f, "B"),
            OperationType::BCAX => write!(f, "BCAX"),
            OperationType::BDEP => write!(f, "BDEP"),
            OperationType::BEXT => write!(f, "BEXT"),
            OperationType::BFC => write!(f, "BFC"),
            OperationType::BFCVT => write!(f, "BFCVT"),
            OperationType::BFCVTN => write!(f, "BFCVTN"),
            OperationType::BFCVTN2 => write!(f, "BFCVTN2"),
            OperationType::BFCVTNT => write!(f, "BFCVTNT"),
            OperationType::BFDOT => write!(f, "BFDOT"),
            OperationType::BFI => write!(f, "BFI"),
            OperationType::BFM => write!(f, "BFM"),
            OperationType::BFMLAL => write!(f, "BFMLAL"),
            OperationType::BFMLALB => write!(f, "BFMLALB"),
            OperationType::BFMLALT => write!(f, "BFMLALT"),
            OperationType::BFMMLA => write!(f, "BFMMLA"),
            OperationType::BFMOPA => write!(f, "BFMOPA"),
            OperationType::BFMOPS => write!(f, "BFMOPS"),
            OperationType::BFXIL => write!(f, "BFXIL"),
            OperationType::BGRP => write!(f, "BGRP"),
            OperationType::BIC => write!(f, "BIC"),
            OperationType::BICS => write!(f, "BICS"),
            OperationType::BIF => write!(f, "BIF"),
            OperationType::BIT => write!(f, "BIT"),
            OperationType::BL => write!(f, "BL"),
            OperationType::BLR => write!(f, "BLR"),
            OperationType::BLRAA => write!(f, "BLRAA"),
            OperationType::BLRAAZ => write!(f, "BLRAAZ"),
            OperationType::BLRAB => write!(f, "BLRAB"),
            OperationType::BLRABZ => write!(f, "BLRABZ"),
            OperationType::BR => write!(f, "BR"),
            OperationType::BRAA => write!(f, "BRAA"),
            OperationType::BRAAZ => write!(f, "BRAAZ"),
            OperationType::BRAB => write!(f, "BRAB"),
            OperationType::BRABZ => write!(f, "BRABZ"),
            OperationType::BRK => write!(f, "BRK"),
            OperationType::BRKA => write!(f, "BRKA"),
            OperationType::BRKAS => write!(f, "BRKAS"),
            OperationType::BRKB => write!(f, "BRKB"),
            OperationType::BRKBS => write!(f, "BRKBS"),
            OperationType::BRKN => write!(f, "BRKN"),
            OperationType::BRKNS => write!(f, "BRKNS"),
            OperationType::BRKPA => write!(f, "BRKPA"),
            OperationType::BRKPAS => write!(f, "BRKPAS"),
            OperationType::BRKPB => write!(f, "BRKPB"),
            OperationType::BRKPBS => write!(f, "BRKPBS"),
            OperationType::BSL => write!(f, "BSL"),
            OperationType::BSL1N => write!(f, "BSL1N"),
            OperationType::BSL2N => write!(f, "BSL2N"),
            OperationType::BTI => write!(f, "BTI"),
            OperationType::B_AL => write!(f, "B_AL"),
            OperationType::B_CC => write!(f, "B_CC"),
            OperationType::B_CS => write!(f, "B_CS"),
            OperationType::B_EQ => write!(f, "B_EQ"),
            OperationType::B_GE => write!(f, "B_GE"),
            OperationType::B_GT => write!(f, "B_GT"),
            OperationType::B_HI => write!(f, "B_HI"),
            OperationType::B_LE => write!(f, "B_LE"),
            OperationType::B_LS => write!(f, "B_LS"),
            OperationType::B_LT => write!(f, "B_LT"),
            OperationType::B_MI => write!(f, "B_MI"),
            OperationType::B_NE => write!(f, "B_NE"),
            OperationType::B_NV => write!(f, "B_NV"),
            OperationType::B_PL => write!(f, "B_PL"),
            OperationType::B_VC => write!(f, "B_VC"),
            OperationType::B_VS => write!(f, "B_VS"),
            OperationType::CADD => write!(f, "CADD"),
            OperationType::CAS => write!(f, "CAS"),
            OperationType::CASA => write!(f, "CASA"),
            OperationType::CASAB => write!(f, "CASAB"),
            OperationType::CASAH => write!(f, "CASAH"),
            OperationType::CASAL => write!(f, "CASAL"),
            OperationType::CASALB => write!(f, "CASALB"),
            OperationType::CASALH => write!(f, "CASALH"),
            OperationType::CASB => write!(f, "CASB"),
            OperationType::CASH => write!(f, "CASH"),
            OperationType::CASL => write!(f, "CASL"),
            OperationType::CASLB => write!(f, "CASLB"),
            OperationType::CASLH => write!(f, "CASLH"),
            OperationType::CASP => write!(f, "CASP"),
            OperationType::CASPA => write!(f, "CASPA"),
            OperationType::CASPAL => write!(f, "CASPAL"),
            OperationType::CASPL => write!(f, "CASPL"),
            OperationType::CBNZ => write!(f, "CBNZ"),
            OperationType::CBZ => write!(f, "CBZ"),
            OperationType::CCMN => write!(f, "CCMN"),
            OperationType::CCMP => write!(f, "CCMP"),
            OperationType::CDOT => write!(f, "CDOT"),
            OperationType::CFINV => write!(f, "CFINV"),
            OperationType::CFP => write!(f, "CFP"),
            OperationType::CINC => write!(f, "CINC"),
            OperationType::CINV => write!(f, "CINV"),
            OperationType::CLASTA => write!(f, "CLASTA"),
            OperationType::CLASTB => write!(f, "CLASTB"),
            OperationType::CLREX => write!(f, "CLREX"),
            OperationType::CLS => write!(f, "CLS"),
            OperationType::CLZ => write!(f, "CLZ"),
            OperationType::CMEQ => write!(f, "CMEQ"),
            OperationType::CMGE => write!(f, "CMGE"),
            OperationType::CMGT => write!(f, "CMGT"),
            OperationType::CMHI => write!(f, "CMHI"),
            OperationType::CMHS => write!(f, "CMHS"),
            OperationType::CMLA => write!(f, "CMLA"),
            OperationType::CMLE => write!(f, "CMLE"),
            OperationType::CMLT => write!(f, "CMLT"),
            OperationType::CMN => write!(f, "CMN"),
            OperationType::CMP => write!(f, "CMP"),
            OperationType::CMPEQ => write!(f, "CMPEQ"),
            OperationType::CMPGE => write!(f, "CMPGE"),
            OperationType::CMPGT => write!(f, "CMPGT"),
            OperationType::CMPHI => write!(f, "CMPHI"),
            OperationType::CMPHS => write!(f, "CMPHS"),
            OperationType::CMPLE => write!(f, "CMPLE"),
            OperationType::CMPLO => write!(f, "CMPLO"),
            OperationType::CMPLS => write!(f, "CMPLS"),
            OperationType::CMPLT => write!(f, "CMPLT"),
            OperationType::CMPNE => write!(f, "CMPNE"),
            OperationType::CMPP => write!(f, "CMPP"),
            OperationType::CMTST => write!(f, "CMTST"),
            OperationType::CNEG => write!(f, "CNEG"),
            OperationType::CNOT => write!(f, "CNOT"),
            OperationType::CNT => write!(f, "CNT"),
            OperationType::CNTB => write!(f, "CNTB"),
            OperationType::CNTD => write!(f, "CNTD"),
            OperationType::CNTH => write!(f, "CNTH"),
            OperationType::CNTP => write!(f, "CNTP"),
            OperationType::CNTW => write!(f, "CNTW"),
            OperationType::COMPACT => write!(f, "COMPACT"),
            OperationType::CPP => write!(f, "CPP"),
            OperationType::CPY => write!(f, "CPY"),
            OperationType::CRC32B => write!(f, "CRC32B"),
            OperationType::CRC32CB => write!(f, "CRC32CB"),
            OperationType::CRC32CH => write!(f, "CRC32CH"),
            OperationType::CRC32CW => write!(f, "CRC32CW"),
            OperationType::CRC32CX => write!(f, "CRC32CX"),
            OperationType::CRC32H => write!(f, "CRC32H"),
            OperationType::CRC32W => write!(f, "CRC32W"),
            OperationType::CRC32X => write!(f, "CRC32X"),
            OperationType::CSDB => write!(f, "CSDB"),
            OperationType::CSEL => write!(f, "CSEL"),
            OperationType::CSET => write!(f, "CSET"),
            OperationType::CSETM => write!(f, "CSETM"),
            OperationType::CSINC => write!(f, "CSINC"),
            OperationType::CSINV => write!(f, "CSINV"),
            OperationType::CSNEG => write!(f, "CSNEG"),
            OperationType::CTERMEQ => write!(f, "CTERMEQ"),
            OperationType::CTERMNE => write!(f, "CTERMNE"),
            OperationType::DC => write!(f, "DC"),
            OperationType::DCPS1 => write!(f, "DCPS1"),
            OperationType::DCPS2 => write!(f, "DCPS2"),
            OperationType::DCPS3 => write!(f, "DCPS3"),
            OperationType::DECB => write!(f, "DECB"),
            OperationType::DECD => write!(f, "DECD"),
            OperationType::DECH => write!(f, "DECH"),
            OperationType::DECP => write!(f, "DECP"),
            OperationType::DECW => write!(f, "DECW"),
            OperationType::DGH => write!(f, "DGH"),
            OperationType::DMB => write!(f, "DMB"),
            OperationType::DRPS => write!(f, "DRPS"),
            OperationType::DSB => write!(f, "DSB"),
            OperationType::DUP => write!(f, "DUP"),
            OperationType::DUPM => write!(f, "DUPM"),
            OperationType::DVP => write!(f, "DVP"),
            OperationType::EON => write!(f, "EON"),
            OperationType::EOR => write!(f, "EOR"),
            OperationType::EOR3 => write!(f, "EOR3"),
            OperationType::EORBT => write!(f, "EORBT"),
            OperationType::EORS => write!(f, "EORS"),
            OperationType::EORTB => write!(f, "EORTB"),
            OperationType::EORV => write!(f, "EORV"),
            OperationType::ERET => write!(f, "ERET"),
            OperationType::ERETAA => write!(f, "ERETAA"),
            OperationType::ERETAB => write!(f, "ERETAB"),
            OperationType::ESB => write!(f, "ESB"),
            OperationType::EXT => write!(f, "EXT"),
            OperationType::EXTR => write!(f, "EXTR"),
            OperationType::FABD => write!(f, "FABD"),
            OperationType::FABS => write!(f, "FABS"),
            OperationType::FACGE => write!(f, "FACGE"),
            OperationType::FACGT => write!(f, "FACGT"),
            OperationType::FACLE => write!(f, "FACLE"),
            OperationType::FACLT => write!(f, "FACLT"),
            OperationType::FADD => write!(f, "FADD"),
            OperationType::FADDA => write!(f, "FADDA"),
            OperationType::FADDP => write!(f, "FADDP"),
            OperationType::FADDV => write!(f, "FADDV"),
            OperationType::FCADD => write!(f, "FCADD"),
            OperationType::FCCMP => write!(f, "FCCMP"),
            OperationType::FCCMPE => write!(f, "FCCMPE"),
            OperationType::FCMEQ => write!(f, "FCMEQ"),
            OperationType::FCMGE => write!(f, "FCMGE"),
            OperationType::FCMGT => write!(f, "FCMGT"),
            OperationType::FCMLA => write!(f, "FCMLA"),
            OperationType::FCMLE => write!(f, "FCMLE"),
            OperationType::FCMLT => write!(f, "FCMLT"),
            OperationType::FCMNE => write!(f, "FCMNE"),
            OperationType::FCMP => write!(f, "FCMP"),
            OperationType::FCMPE => write!(f, "FCMPE"),
            OperationType::FCMUO => write!(f, "FCMUO"),
            OperationType::FCPY => write!(f, "FCPY"),
            OperationType::FCSEL => write!(f, "FCSEL"),
            OperationType::FCVT => write!(f, "FCVT"),
            OperationType::FCVTAS => write!(f, "FCVTAS"),
            OperationType::FCVTAU => write!(f, "FCVTAU"),
            OperationType::FCVTL => write!(f, "FCVTL"),
            OperationType::FCVTL2 => write!(f, "FCVTL2"),
            OperationType::FCVTLT => write!(f, "FCVTLT"),
            OperationType::FCVTMS => write!(f, "FCVTMS"),
            OperationType::FCVTMU => write!(f, "FCVTMU"),
            OperationType::FCVTN => write!(f, "FCVTN"),
            OperationType::FCVTN2 => write!(f, "FCVTN2"),
            OperationType::FCVTNS => write!(f, "FCVTNS"),
            OperationType::FCVTNT => write!(f, "FCVTNT"),
            OperationType::FCVTNU => write!(f, "FCVTNU"),
            OperationType::FCVTPS => write!(f, "FCVTPS"),
            OperationType::FCVTPU => write!(f, "FCVTPU"),
            OperationType::FCVTX => write!(f, "FCVTX"),
            OperationType::FCVTXN => write!(f, "FCVTXN"),
            OperationType::FCVTXN2 => write!(f, "FCVTXN2"),
            OperationType::FCVTXNT => write!(f, "FCVTXNT"),
            OperationType::FCVTZS => write!(f, "FCVTZS"),
            OperationType::FCVTZU => write!(f, "FCVTZU"),
            OperationType::FDIV => write!(f, "FDIV"),
            OperationType::FDIVR => write!(f, "FDIVR"),
            OperationType::FDUP => write!(f, "FDUP"),
            OperationType::FEXPA => write!(f, "FEXPA"),
            OperationType::FJCVTZS => write!(f, "FJCVTZS"),
            OperationType::FLOGB => write!(f, "FLOGB"),
            OperationType::FMAD => write!(f, "FMAD"),
            OperationType::FMADD => write!(f, "FMADD"),
            OperationType::FMAX => write!(f, "FMAX"),
            OperationType::FMAXNM => write!(f, "FMAXNM"),
            OperationType::FMAXNMP => write!(f, "FMAXNMP"),
            OperationType::FMAXNMV => write!(f, "FMAXNMV"),
            OperationType::FMAXP => write!(f, "FMAXP"),
            OperationType::FMAXV => write!(f, "FMAXV"),
            OperationType::FMIN => write!(f, "FMIN"),
            OperationType::FMINNM => write!(f, "FMINNM"),
            OperationType::FMINNMP => write!(f, "FMINNMP"),
            OperationType::FMINNMV => write!(f, "FMINNMV"),
            OperationType::FMINP => write!(f, "FMINP"),
            OperationType::FMINV => write!(f, "FMINV"),
            OperationType::FMLA => write!(f, "FMLA"),
            OperationType::FMLAL => write!(f, "FMLAL"),
            OperationType::FMLAL2 => write!(f, "FMLAL2"),
            OperationType::FMLALB => write!(f, "FMLALB"),
            OperationType::FMLALT => write!(f, "FMLALT"),
            OperationType::FMLS => write!(f, "FMLS"),
            OperationType::FMLSL => write!(f, "FMLSL"),
            OperationType::FMLSL2 => write!(f, "FMLSL2"),
            OperationType::FMLSLB => write!(f, "FMLSLB"),
            OperationType::FMLSLT => write!(f, "FMLSLT"),
            OperationType::FMMLA => write!(f, "FMMLA"),
            OperationType::FMOPA => write!(f, "FMOPA"),
            OperationType::FMOPS => write!(f, "FMOPS"),
            OperationType::FMOV => write!(f, "FMOV"),
            OperationType::FMSB => write!(f, "FMSB"),
            OperationType::FMSUB => write!(f, "FMSUB"),
            OperationType::FMUL => write!(f, "FMUL"),
            OperationType::FMULX => write!(f, "FMULX"),
            OperationType::FNEG => write!(f, "FNEG"),
            OperationType::FNMAD => write!(f, "FNMAD"),
            OperationType::FNMADD => write!(f, "FNMADD"),
            OperationType::FNMLA => write!(f, "FNMLA"),
            OperationType::FNMLS => write!(f, "FNMLS"),
            OperationType::FNMSB => write!(f, "FNMSB"),
            OperationType::FNMSUB => write!(f, "FNMSUB"),
            OperationType::FNMUL => write!(f, "FNMUL"),
            OperationType::FRECPE => write!(f, "FRECPE"),
            OperationType::FRECPS => write!(f, "FRECPS"),
            OperationType::FRECPX => write!(f, "FRECPX"),
            OperationType::FRINT32X => write!(f, "FRINT32X"),
            OperationType::FRINT32Z => write!(f, "FRINT32Z"),
            OperationType::FRINT64X => write!(f, "FRINT64X"),
            OperationType::FRINT64Z => write!(f, "FRINT64Z"),
            OperationType::FRINTA => write!(f, "FRINTA"),
            OperationType::FRINTI => write!(f, "FRINTI"),
            OperationType::FRINTM => write!(f, "FRINTM"),
            OperationType::FRINTN => write!(f, "FRINTN"),
            OperationType::FRINTP => write!(f, "FRINTP"),
            OperationType::FRINTX => write!(f, "FRINTX"),
            OperationType::FRINTZ => write!(f, "FRINTZ"),
            OperationType::FRSQRTE => write!(f, "FRSQRTE"),
            OperationType::FRSQRTS => write!(f, "FRSQRTS"),
            OperationType::FSCALE => write!(f, "FSCALE"),
            OperationType::FSQRT => write!(f, "FSQRT"),
            OperationType::FSUB => write!(f, "FSUB"),
            OperationType::FSUBR => write!(f, "FSUBR"),
            OperationType::FTMAD => write!(f, "FTMAD"),
            OperationType::FTSMUL => write!(f, "FTSMUL"),
            OperationType::FTSSEL => write!(f, "FTSSEL"),
            OperationType::GMI => write!(f, "GMI"),
            OperationType::HINT => write!(f, "HINT"),
            OperationType::HISTCNT => write!(f, "HISTCNT"),
            OperationType::HISTSEG => write!(f, "HISTSEG"),
            OperationType::HLT => write!(f, "HLT"),
            OperationType::HVC => write!(f, "HVC"),
            OperationType::IC => write!(f, "IC"),
            OperationType::INCB => write!(f, "INCB"),
            OperationType::INCD => write!(f, "INCD"),
            OperationType::INCH => write!(f, "INCH"),
            OperationType::INCP => write!(f, "INCP"),
            OperationType::INCW => write!(f, "INCW"),
            OperationType::INDEX => write!(f, "INDEX"),
            OperationType::INS => write!(f, "INS"),
            OperationType::INSR => write!(f, "INSR"),
            OperationType::IRG => write!(f, "IRG"),
            OperationType::ISB => write!(f, "ISB"),
            OperationType::LASTA => write!(f, "LASTA"),
            OperationType::LASTB => write!(f, "LASTB"),
            OperationType::LD1 => write!(f, "LD1"),
            OperationType::LD1B => write!(f, "LD1B"),
            OperationType::LD1D => write!(f, "LD1D"),
            OperationType::LD1H => write!(f, "LD1H"),
            OperationType::LD1Q => write!(f, "LD1Q"),
            OperationType::LD1R => write!(f, "LD1R"),
            OperationType::LD1RB => write!(f, "LD1RB"),
            OperationType::LD1RD => write!(f, "LD1RD"),
            OperationType::LD1RH => write!(f, "LD1RH"),
            OperationType::LD1ROB => write!(f, "LD1ROB"),
            OperationType::LD1ROD => write!(f, "LD1ROD"),
            OperationType::LD1ROH => write!(f, "LD1ROH"),
            OperationType::LD1ROW => write!(f, "LD1ROW"),
            OperationType::LD1RQB => write!(f, "LD1RQB"),
            OperationType::LD1RQD => write!(f, "LD1RQD"),
            OperationType::LD1RQH => write!(f, "LD1RQH"),
            OperationType::LD1RQW => write!(f, "LD1RQW"),
            OperationType::LD1RSB => write!(f, "LD1RSB"),
            OperationType::LD1RSH => write!(f, "LD1RSH"),
            OperationType::LD1RSW => write!(f, "LD1RSW"),
            OperationType::LD1RW => write!(f, "LD1RW"),
            OperationType::LD1SB => write!(f, "LD1SB"),
            OperationType::LD1SH => write!(f, "LD1SH"),
            OperationType::LD1SW => write!(f, "LD1SW"),
            OperationType::LD1W => write!(f, "LD1W"),
            OperationType::LD2 => write!(f, "LD2"),
            OperationType::LD2B => write!(f, "LD2B"),
            OperationType::LD2D => write!(f, "LD2D"),
            OperationType::LD2H => write!(f, "LD2H"),
            OperationType::LD2R => write!(f, "LD2R"),
            OperationType::LD2W => write!(f, "LD2W"),
            OperationType::LD3 => write!(f, "LD3"),
            OperationType::LD3B => write!(f, "LD3B"),
            OperationType::LD3D => write!(f, "LD3D"),
            OperationType::LD3H => write!(f, "LD3H"),
            OperationType::LD3R => write!(f, "LD3R"),
            OperationType::LD3W => write!(f, "LD3W"),
            OperationType::LD4 => write!(f, "LD4"),
            OperationType::LD4B => write!(f, "LD4B"),
            OperationType::LD4D => write!(f, "LD4D"),
            OperationType::LD4H => write!(f, "LD4H"),
            OperationType::LD4R => write!(f, "LD4R"),
            OperationType::LD4W => write!(f, "LD4W"),
            OperationType::LD64B => write!(f, "LD64B"),
            OperationType::LDADD => write!(f, "LDADD"),
            OperationType::LDADDA => write!(f, "LDADDA"),
            OperationType::LDADDAB => write!(f, "LDADDAB"),
            OperationType::LDADDAH => write!(f, "LDADDAH"),
            OperationType::LDADDAL => write!(f, "LDADDAL"),
            OperationType::LDADDALB => write!(f, "LDADDALB"),
            OperationType::LDADDALH => write!(f, "LDADDALH"),
            OperationType::LDADDB => write!(f, "LDADDB"),
            OperationType::LDADDH => write!(f, "LDADDH"),
            OperationType::LDADDL => write!(f, "LDADDL"),
            OperationType::LDADDLB => write!(f, "LDADDLB"),
            OperationType::LDADDLH => write!(f, "LDADDLH"),
            OperationType::LDAPR => write!(f, "LDAPR"),
            OperationType::LDAPRB => write!(f, "LDAPRB"),
            OperationType::LDAPRH => write!(f, "LDAPRH"),
            OperationType::LDAPUR => write!(f, "LDAPUR"),
            OperationType::LDAPURB => write!(f, "LDAPURB"),
            OperationType::LDAPURH => write!(f, "LDAPURH"),
            OperationType::LDAPURSB => write!(f, "LDAPURSB"),
            OperationType::LDAPURSH => write!(f, "LDAPURSH"),
            OperationType::LDAPURSW => write!(f, "LDAPURSW"),
            OperationType::LDAR => write!(f, "LDAR"),
            OperationType::LDARB => write!(f, "LDARB"),
            OperationType::LDARH => write!(f, "LDARH"),
            OperationType::LDAXP => write!(f, "LDAXP"),
            OperationType::LDAXR => write!(f, "LDAXR"),
            OperationType::LDAXRB => write!(f, "LDAXRB"),
            OperationType::LDAXRH => write!(f, "LDAXRH"),
            OperationType::LDCLR => write!(f, "LDCLR"),
            OperationType::LDCLRA => write!(f, "LDCLRA"),
            OperationType::LDCLRAB => write!(f, "LDCLRAB"),
            OperationType::LDCLRAH => write!(f, "LDCLRAH"),
            OperationType::LDCLRAL => write!(f, "LDCLRAL"),
            OperationType::LDCLRALB => write!(f, "LDCLRALB"),
            OperationType::LDCLRALH => write!(f, "LDCLRALH"),
            OperationType::LDCLRB => write!(f, "LDCLRB"),
            OperationType::LDCLRH => write!(f, "LDCLRH"),
            OperationType::LDCLRL => write!(f, "LDCLRL"),
            OperationType::LDCLRLB => write!(f, "LDCLRLB"),
            OperationType::LDCLRLH => write!(f, "LDCLRLH"),
            OperationType::LDEOR => write!(f, "LDEOR"),
            OperationType::LDEORA => write!(f, "LDEORA"),
            OperationType::LDEORAB => write!(f, "LDEORAB"),
            OperationType::LDEORAH => write!(f, "LDEORAH"),
            OperationType::LDEORAL => write!(f, "LDEORAL"),
            OperationType::LDEORALB => write!(f, "LDEORALB"),
            OperationType::LDEORALH => write!(f, "LDEORALH"),
            OperationType::LDEORB => write!(f, "LDEORB"),
            OperationType::LDEORH => write!(f, "LDEORH"),
            OperationType::LDEORL => write!(f, "LDEORL"),
            OperationType::LDEORLB => write!(f, "LDEORLB"),
            OperationType::LDEORLH => write!(f, "LDEORLH"),
            OperationType::LDFF1B => write!(f, "LDFF1B"),
            OperationType::LDFF1D => write!(f, "LDFF1D"),
            OperationType::LDFF1H => write!(f, "LDFF1H"),
            OperationType::LDFF1SB => write!(f, "LDFF1SB"),
            OperationType::LDFF1SH => write!(f, "LDFF1SH"),
            OperationType::LDFF1SW => write!(f, "LDFF1SW"),
            OperationType::LDFF1W => write!(f, "LDFF1W"),
            OperationType::LDG => write!(f, "LDG"),
            OperationType::LDGM => write!(f, "LDGM"),
            OperationType::LDLAR => write!(f, "LDLAR"),
            OperationType::LDLARB => write!(f, "LDLARB"),
            OperationType::LDLARH => write!(f, "LDLARH"),
            OperationType::LDNF1B => write!(f, "LDNF1B"),
            OperationType::LDNF1D => write!(f, "LDNF1D"),
            OperationType::LDNF1H => write!(f, "LDNF1H"),
            OperationType::LDNF1SB => write!(f, "LDNF1SB"),
            OperationType::LDNF1SH => write!(f, "LDNF1SH"),
            OperationType::LDNF1SW => write!(f, "LDNF1SW"),
            OperationType::LDNF1W => write!(f, "LDNF1W"),
            OperationType::LDNP => write!(f, "LDNP"),
            OperationType::LDNT1B => write!(f, "LDNT1B"),
            OperationType::LDNT1D => write!(f, "LDNT1D"),
            OperationType::LDNT1H => write!(f, "LDNT1H"),
            OperationType::LDNT1SB => write!(f, "LDNT1SB"),
            OperationType::LDNT1SH => write!(f, "LDNT1SH"),
            OperationType::LDNT1SW => write!(f, "LDNT1SW"),
            OperationType::LDNT1W => write!(f, "LDNT1W"),
            OperationType::LDP => write!(f, "LDP"),
            OperationType::LDPSW => write!(f, "LDPSW"),
            OperationType::LDR => write!(f, "LDR"),
            OperationType::LDRAA => write!(f, "LDRAA"),
            OperationType::LDRAB => write!(f, "LDRAB"),
            OperationType::LDRB => write!(f, "LDRB"),
            OperationType::LDRH => write!(f, "LDRH"),
            OperationType::LDRSB => write!(f, "LDRSB"),
            OperationType::LDRSH => write!(f, "LDRSH"),
            OperationType::LDRSW => write!(f, "LDRSW"),
            OperationType::LDSET => write!(f, "LDSET"),
            OperationType::LDSETA => write!(f, "LDSETA"),
            OperationType::LDSETAB => write!(f, "LDSETAB"),
            OperationType::LDSETAH => write!(f, "LDSETAH"),
            OperationType::LDSETAL => write!(f, "LDSETAL"),
            OperationType::LDSETALB => write!(f, "LDSETALB"),
            OperationType::LDSETALH => write!(f, "LDSETALH"),
            OperationType::LDSETB => write!(f, "LDSETB"),
            OperationType::LDSETH => write!(f, "LDSETH"),
            OperationType::LDSETL => write!(f, "LDSETL"),
            OperationType::LDSETLB => write!(f, "LDSETLB"),
            OperationType::LDSETLH => write!(f, "LDSETLH"),
            OperationType::LDSMAX => write!(f, "LDSMAX"),
            OperationType::LDSMAXA => write!(f, "LDSMAXA"),
            OperationType::LDSMAXAB => write!(f, "LDSMAXAB"),
            OperationType::LDSMAXAH => write!(f, "LDSMAXAH"),
            OperationType::LDSMAXAL => write!(f, "LDSMAXAL"),
            OperationType::LDSMAXALB => write!(f, "LDSMAXALB"),
            OperationType::LDSMAXALH => write!(f, "LDSMAXALH"),
            OperationType::LDSMAXB => write!(f, "LDSMAXB"),
            OperationType::LDSMAXH => write!(f, "LDSMAXH"),
            OperationType::LDSMAXL => write!(f, "LDSMAXL"),
            OperationType::LDSMAXLB => write!(f, "LDSMAXLB"),
            OperationType::LDSMAXLH => write!(f, "LDSMAXLH"),
            OperationType::LDSMIN => write!(f, "LDSMIN"),
            OperationType::LDSMINA => write!(f, "LDSMINA"),
            OperationType::LDSMINAB => write!(f, "LDSMINAB"),
            OperationType::LDSMINAH => write!(f, "LDSMINAH"),
            OperationType::LDSMINAL => write!(f, "LDSMINAL"),
            OperationType::LDSMINALB => write!(f, "LDSMINALB"),
            OperationType::LDSMINALH => write!(f, "LDSMINALH"),
            OperationType::LDSMINB => write!(f, "LDSMINB"),
            OperationType::LDSMINH => write!(f, "LDSMINH"),
            OperationType::LDSMINL => write!(f, "LDSMINL"),
            OperationType::LDSMINLB => write!(f, "LDSMINLB"),
            OperationType::LDSMINLH => write!(f, "LDSMINLH"),
            OperationType::LDTR => write!(f, "LDTR"),
            OperationType::LDTRB => write!(f, "LDTRB"),
            OperationType::LDTRH => write!(f, "LDTRH"),
            OperationType::LDTRSB => write!(f, "LDTRSB"),
            OperationType::LDTRSH => write!(f, "LDTRSH"),
            OperationType::LDTRSW => write!(f, "LDTRSW"),
            OperationType::LDUMAX => write!(f, "LDUMAX"),
            OperationType::LDUMAXA => write!(f, "LDUMAXA"),
            OperationType::LDUMAXAB => write!(f, "LDUMAXAB"),
            OperationType::LDUMAXAH => write!(f, "LDUMAXAH"),
            OperationType::LDUMAXAL => write!(f, "LDUMAXAL"),
            OperationType::LDUMAXALB => write!(f, "LDUMAXALB"),
            OperationType::LDUMAXALH => write!(f, "LDUMAXALH"),
            OperationType::LDUMAXB => write!(f, "LDUMAXB"),
            OperationType::LDUMAXH => write!(f, "LDUMAXH"),
            OperationType::LDUMAXL => write!(f, "LDUMAXL"),
            OperationType::LDUMAXLB => write!(f, "LDUMAXLB"),
            OperationType::LDUMAXLH => write!(f, "LDUMAXLH"),
            OperationType::LDUMIN => write!(f, "LDUMIN"),
            OperationType::LDUMINA => write!(f, "LDUMINA"),
            OperationType::LDUMINAB => write!(f, "LDUMINAB"),
            OperationType::LDUMINAH => write!(f, "LDUMINAH"),
            OperationType::LDUMINAL => write!(f, "LDUMINAL"),
            OperationType::LDUMINALB => write!(f, "LDUMINALB"),
            OperationType::LDUMINALH => write!(f, "LDUMINALH"),
            OperationType::LDUMINB => write!(f, "LDUMINB"),
            OperationType::LDUMINH => write!(f, "LDUMINH"),
            OperationType::LDUMINL => write!(f, "LDUMINL"),
            OperationType::LDUMINLB => write!(f, "LDUMINLB"),
            OperationType::LDUMINLH => write!(f, "LDUMINLH"),
            OperationType::LDUR => write!(f, "LDUR"),
            OperationType::LDURB => write!(f, "LDURB"),
            OperationType::LDURH => write!(f, "LDURH"),
            OperationType::LDURSB => write!(f, "LDURSB"),
            OperationType::LDURSH => write!(f, "LDURSH"),
            OperationType::LDURSW => write!(f, "LDURSW"),
            OperationType::LDXP => write!(f, "LDXP"),
            OperationType::LDXR => write!(f, "LDXR"),
            OperationType::LDXRB => write!(f, "LDXRB"),
            OperationType::LDXRH => write!(f, "LDXRH"),
            OperationType::LSL => write!(f, "LSL"),
            OperationType::LSLR => write!(f, "LSLR"),
            OperationType::LSLV => write!(f, "LSLV"),
            OperationType::LSR => write!(f, "LSR"),
            OperationType::LSRR => write!(f, "LSRR"),
            OperationType::LSRV => write!(f, "LSRV"),
            OperationType::MAD => write!(f, "MAD"),
            OperationType::MADD => write!(f, "MADD"),
            OperationType::MATCH => write!(f, "MATCH"),
            OperationType::MLA => write!(f, "MLA"),
            OperationType::MLS => write!(f, "MLS"),
            OperationType::MNEG => write!(f, "MNEG"),
            OperationType::MOV => write!(f, "MOV"),
            OperationType::MOVA => write!(f, "MOVA"),
            OperationType::MOVI => write!(f, "MOVI"),
            OperationType::MOVK => write!(f, "MOVK"),
            OperationType::MOVN => write!(f, "MOVN"),
            OperationType::MOVPRFX => write!(f, "MOVPRFX"),
            OperationType::MOVS => write!(f, "MOVS"),
            OperationType::MOVZ => write!(f, "MOVZ"),
            OperationType::MRS => write!(f, "MRS"),
            OperationType::MSB => write!(f, "MSB"),
            OperationType::MSR => write!(f, "MSR"),
            OperationType::MSUB => write!(f, "MSUB"),
            OperationType::MUL => write!(f, "MUL"),
            OperationType::MVN => write!(f, "MVN"),
            OperationType::MVNI => write!(f, "MVNI"),
            OperationType::NAND => write!(f, "NAND"),
            OperationType::NANDS => write!(f, "NANDS"),
            OperationType::NBSL => write!(f, "NBSL"),
            OperationType::NEG => write!(f, "NEG"),
            OperationType::NEGS => write!(f, "NEGS"),
            OperationType::NGC => write!(f, "NGC"),
            OperationType::NGCS => write!(f, "NGCS"),
            OperationType::NMATCH => write!(f, "NMATCH"),
            OperationType::NOP => write!(f, "NOP"),
            OperationType::NOR => write!(f, "NOR"),
            OperationType::NORS => write!(f, "NORS"),
            OperationType::NOT => write!(f, "NOT"),
            OperationType::NOTS => write!(f, "NOTS"),
            OperationType::ORN => write!(f, "ORN"),
            OperationType::ORNS => write!(f, "ORNS"),
            OperationType::ORR => write!(f, "ORR"),
            OperationType::ORRS => write!(f, "ORRS"),
            OperationType::ORV => write!(f, "ORV"),
            OperationType::PACDA => write!(f, "PACDA"),
            OperationType::PACDB => write!(f, "PACDB"),
            OperationType::PACDZA => write!(f, "PACDZA"),
            OperationType::PACDZB => write!(f, "PACDZB"),
            OperationType::PACGA => write!(f, "PACGA"),
            OperationType::PACIA => write!(f, "PACIA"),
            OperationType::PACIA1716 => write!(f, "PACIA1716"),
            OperationType::PACIASP => write!(f, "PACIASP"),
            OperationType::PACIAZ => write!(f, "PACIAZ"),
            OperationType::PACIB => write!(f, "PACIB"),
            OperationType::PACIB1716 => write!(f, "PACIB1716"),
            OperationType::PACIBSP => write!(f, "PACIBSP"),
            OperationType::PACIBZ => write!(f, "PACIBZ"),
            OperationType::PACIZA => write!(f, "PACIZA"),
            OperationType::PACIZB => write!(f, "PACIZB"),
            OperationType::PFALSE => write!(f, "PFALSE"),
            OperationType::PFIRST => write!(f, "PFIRST"),
            OperationType::PMUL => write!(f, "PMUL"),
            OperationType::PMULL => write!(f, "PMULL"),
            OperationType::PMULL2 => write!(f, "PMULL2"),
            OperationType::PMULLB => write!(f, "PMULLB"),
            OperationType::PMULLT => write!(f, "PMULLT"),
            OperationType::PNEXT => write!(f, "PNEXT"),
            OperationType::PRFB => write!(f, "PRFB"),
            OperationType::PRFD => write!(f, "PRFD"),
            OperationType::PRFH => write!(f, "PRFH"),
            OperationType::PRFM => write!(f, "PRFM"),
            OperationType::PRFUM => write!(f, "PRFUM"),
            OperationType::PRFW => write!(f, "PRFW"),
            OperationType::PSB => write!(f, "PSB"),
            OperationType::PSSBB => write!(f, "PSSBB"),
            OperationType::PTEST => write!(f, "PTEST"),
            OperationType::PTRUE => write!(f, "PTRUE"),
            OperationType::PTRUES => write!(f, "PTRUES"),
            OperationType::PUNPKHI => write!(f, "PUNPKHI"),
            OperationType::PUNPKLO => write!(f, "PUNPKLO"),
            OperationType::RADDHN => write!(f, "RADDHN"),
            OperationType::RADDHN2 => write!(f, "RADDHN2"),
            OperationType::RADDHNB => write!(f, "RADDHNB"),
            OperationType::RADDHNT => write!(f, "RADDHNT"),
            OperationType::RAX1 => write!(f, "RAX1"),
            OperationType::RBIT => write!(f, "RBIT"),
            OperationType::RDFFR => write!(f, "RDFFR"),
            OperationType::RDFFRS => write!(f, "RDFFRS"),
            OperationType::RDVL => write!(f, "RDVL"),
            OperationType::RET => write!(f, "RET"),
            OperationType::RETAA => write!(f, "RETAA"),
            OperationType::RETAB => write!(f, "RETAB"),
            OperationType::REV => write!(f, "REV"),
            OperationType::REV16 => write!(f, "REV16"),
            OperationType::REV32 => write!(f, "REV32"),
            OperationType::REV64 => write!(f, "REV64"),
            OperationType::REVB => write!(f, "REVB"),
            OperationType::REVD => write!(f, "REVD"),
            OperationType::REVH => write!(f, "REVH"),
            OperationType::REVW => write!(f, "REVW"),
            OperationType::RMIF => write!(f, "RMIF"),
            OperationType::ROR => write!(f, "ROR"),
            OperationType::RORV => write!(f, "RORV"),
            OperationType::RSHRN => write!(f, "RSHRN"),
            OperationType::RSHRN2 => write!(f, "RSHRN2"),
            OperationType::RSHRNB => write!(f, "RSHRNB"),
            OperationType::RSHRNT => write!(f, "RSHRNT"),
            OperationType::RSUBHN => write!(f, "RSUBHN"),
            OperationType::RSUBHN2 => write!(f, "RSUBHN2"),
            OperationType::RSUBHNB => write!(f, "RSUBHNB"),
            OperationType::RSUBHNT => write!(f, "RSUBHNT"),
            OperationType::SABA => write!(f, "SABA"),
            OperationType::SABAL => write!(f, "SABAL"),
            OperationType::SABAL2 => write!(f, "SABAL2"),
            OperationType::SABALB => write!(f, "SABALB"),
            OperationType::SABALT => write!(f, "SABALT"),
            OperationType::SABD => write!(f, "SABD"),
            OperationType::SABDL => write!(f, "SABDL"),
            OperationType::SABDL2 => write!(f, "SABDL2"),
            OperationType::SABDLB => write!(f, "SABDLB"),
            OperationType::SABDLT => write!(f, "SABDLT"),
            OperationType::SADALP => write!(f, "SADALP"),
            OperationType::SADDL => write!(f, "SADDL"),
            OperationType::SADDL2 => write!(f, "SADDL2"),
            OperationType::SADDLB => write!(f, "SADDLB"),
            OperationType::SADDLBT => write!(f, "SADDLBT"),
            OperationType::SADDLP => write!(f, "SADDLP"),
            OperationType::SADDLT => write!(f, "SADDLT"),
            OperationType::SADDLV => write!(f, "SADDLV"),
            OperationType::SADDV => write!(f, "SADDV"),
            OperationType::SADDW => write!(f, "SADDW"),
            OperationType::SADDW2 => write!(f, "SADDW2"),
            OperationType::SADDWB => write!(f, "SADDWB"),
            OperationType::SADDWT => write!(f, "SADDWT"),
            OperationType::SB => write!(f, "SB"),
            OperationType::SBC => write!(f, "SBC"),
            OperationType::SBCLB => write!(f, "SBCLB"),
            OperationType::SBCLT => write!(f, "SBCLT"),
            OperationType::SBCS => write!(f, "SBCS"),
            OperationType::SBFIZ => write!(f, "SBFIZ"),
            OperationType::SBFM => write!(f, "SBFM"),
            OperationType::SBFX => write!(f, "SBFX"),
            OperationType::SCLAMP => write!(f, "SCLAMP"),
            OperationType::SCVTF => write!(f, "SCVTF"),
            OperationType::SDIV => write!(f, "SDIV"),
            OperationType::SDIVR => write!(f, "SDIVR"),
            OperationType::SDOT => write!(f, "SDOT"),
            OperationType::SEL => write!(f, "SEL"),
            OperationType::SETF16 => write!(f, "SETF16"),
            OperationType::SETF8 => write!(f, "SETF8"),
            OperationType::SETFFR => write!(f, "SETFFR"),
            OperationType::SEV => write!(f, "SEV"),
            OperationType::SEVL => write!(f, "SEVL"),
            OperationType::SHA1C => write!(f, "SHA1C"),
            OperationType::SHA1H => write!(f, "SHA1H"),
            OperationType::SHA1M => write!(f, "SHA1M"),
            OperationType::SHA1P => write!(f, "SHA1P"),
            OperationType::SHA1SU0 => write!(f, "SHA1SU0"),
            OperationType::SHA1SU1 => write!(f, "SHA1SU1"),
            OperationType::SHA256H => write!(f, "SHA256H"),
            OperationType::SHA256H2 => write!(f, "SHA256H2"),
            OperationType::SHA256SU0 => write!(f, "SHA256SU0"),
            OperationType::SHA256SU1 => write!(f, "SHA256SU1"),
            OperationType::SHA512H => write!(f, "SHA512H"),
            OperationType::SHA512H2 => write!(f, "SHA512H2"),
            OperationType::SHA512SU0 => write!(f, "SHA512SU0"),
            OperationType::SHA512SU1 => write!(f, "SHA512SU1"),
            OperationType::SHADD => write!(f, "SHADD"),
            OperationType::SHL => write!(f, "SHL"),
            OperationType::SHLL => write!(f, "SHLL"),
            OperationType::SHLL2 => write!(f, "SHLL2"),
            OperationType::SHRN => write!(f, "SHRN"),
            OperationType::SHRN2 => write!(f, "SHRN2"),
            OperationType::SHRNB => write!(f, "SHRNB"),
            OperationType::SHRNT => write!(f, "SHRNT"),
            OperationType::SHSUB => write!(f, "SHSUB"),
            OperationType::SHSUBR => write!(f, "SHSUBR"),
            OperationType::SLI => write!(f, "SLI"),
            OperationType::SM3PARTW1 => write!(f, "SM3PARTW1"),
            OperationType::SM3PARTW2 => write!(f, "SM3PARTW2"),
            OperationType::SM3SS1 => write!(f, "SM3SS1"),
            OperationType::SM3TT1A => write!(f, "SM3TT1A"),
            OperationType::SM3TT1B => write!(f, "SM3TT1B"),
            OperationType::SM3TT2A => write!(f, "SM3TT2A"),
            OperationType::SM3TT2B => write!(f, "SM3TT2B"),
            OperationType::SM4E => write!(f, "SM4E"),
            OperationType::SM4EKEY => write!(f, "SM4EKEY"),
            OperationType::SMADDL => write!(f, "SMADDL"),
            OperationType::SMAX => write!(f, "SMAX"),
            OperationType::SMAXP => write!(f, "SMAXP"),
            OperationType::SMAXV => write!(f, "SMAXV"),
            OperationType::SMC => write!(f, "SMC"),
            OperationType::SMIN => write!(f, "SMIN"),
            OperationType::SMINP => write!(f, "SMINP"),
            OperationType::SMINV => write!(f, "SMINV"),
            OperationType::SMLAL => write!(f, "SMLAL"),
            OperationType::SMLAL2 => write!(f, "SMLAL2"),
            OperationType::SMLALB => write!(f, "SMLALB"),
            OperationType::SMLALT => write!(f, "SMLALT"),
            OperationType::SMLSL => write!(f, "SMLSL"),
            OperationType::SMLSL2 => write!(f, "SMLSL2"),
            OperationType::SMLSLB => write!(f, "SMLSLB"),
            OperationType::SMLSLT => write!(f, "SMLSLT"),
            OperationType::SMMLA => write!(f, "SMMLA"),
            OperationType::SMNEGL => write!(f, "SMNEGL"),
            OperationType::SMOPA => write!(f, "SMOPA"),
            OperationType::SMOPS => write!(f, "SMOPS"),
            OperationType::SMOV => write!(f, "SMOV"),
            OperationType::SMSTART => write!(f, "SMSTART"),
            OperationType::SMSTOP => write!(f, "SMSTOP"),
            OperationType::SMSUBL => write!(f, "SMSUBL"),
            OperationType::SMULH => write!(f, "SMULH"),
            OperationType::SMULL => write!(f, "SMULL"),
            OperationType::SMULL2 => write!(f, "SMULL2"),
            OperationType::SMULLB => write!(f, "SMULLB"),
            OperationType::SMULLT => write!(f, "SMULLT"),
            OperationType::SPLICE => write!(f, "SPLICE"),
            OperationType::SQABS => write!(f, "SQABS"),
            OperationType::SQADD => write!(f, "SQADD"),
            OperationType::SQCADD => write!(f, "SQCADD"),
            OperationType::SQDECB => write!(f, "SQDECB"),
            OperationType::SQDECD => write!(f, "SQDECD"),
            OperationType::SQDECH => write!(f, "SQDECH"),
            OperationType::SQDECP => write!(f, "SQDECP"),
            OperationType::SQDECW => write!(f, "SQDECW"),
            OperationType::SQDMLAL => write!(f, "SQDMLAL"),
            OperationType::SQDMLAL2 => write!(f, "SQDMLAL2"),
            OperationType::SQDMLALB => write!(f, "SQDMLALB"),
            OperationType::SQDMLALBT => write!(f, "SQDMLALBT"),
            OperationType::SQDMLALT => write!(f, "SQDMLALT"),
            OperationType::SQDMLSL => write!(f, "SQDMLSL"),
            OperationType::SQDMLSL2 => write!(f, "SQDMLSL2"),
            OperationType::SQDMLSLB => write!(f, "SQDMLSLB"),
            OperationType::SQDMLSLBT => write!(f, "SQDMLSLBT"),
            OperationType::SQDMLSLT => write!(f, "SQDMLSLT"),
            OperationType::SQDMULH => write!(f, "SQDMULH"),
            OperationType::SQDMULL => write!(f, "SQDMULL"),
            OperationType::SQDMULL2 => write!(f, "SQDMULL2"),
            OperationType::SQDMULLB => write!(f, "SQDMULLB"),
            OperationType::SQDMULLT => write!(f, "SQDMULLT"),
            OperationType::SQINCB => write!(f, "SQINCB"),
            OperationType::SQINCD => write!(f, "SQINCD"),
            OperationType::SQINCH => write!(f, "SQINCH"),
            OperationType::SQINCP => write!(f, "SQINCP"),
            OperationType::SQINCW => write!(f, "SQINCW"),
            OperationType::SQNEG => write!(f, "SQNEG"),
            OperationType::SQRDCMLAH => write!(f, "SQRDCMLAH"),
            OperationType::SQRDMLAH => write!(f, "SQRDMLAH"),
            OperationType::SQRDMLSH => write!(f, "SQRDMLSH"),
            OperationType::SQRDMULH => write!(f, "SQRDMULH"),
            OperationType::SQRSHL => write!(f, "SQRSHL"),
            OperationType::SQRSHLR => write!(f, "SQRSHLR"),
            OperationType::SQRSHRN => write!(f, "SQRSHRN"),
            OperationType::SQRSHRN2 => write!(f, "SQRSHRN2"),
            OperationType::SQRSHRNB => write!(f, "SQRSHRNB"),
            OperationType::SQRSHRNT => write!(f, "SQRSHRNT"),
            OperationType::SQRSHRUN => write!(f, "SQRSHRUN"),
            OperationType::SQRSHRUN2 => write!(f, "SQRSHRUN2"),
            OperationType::SQRSHRUNB => write!(f, "SQRSHRUNB"),
            OperationType::SQRSHRUNT => write!(f, "SQRSHRUNT"),
            OperationType::SQSHL => write!(f, "SQSHL"),
            OperationType::SQSHLR => write!(f, "SQSHLR"),
            OperationType::SQSHLU => write!(f, "SQSHLU"),
            OperationType::SQSHRN => write!(f, "SQSHRN"),
            OperationType::SQSHRN2 => write!(f, "SQSHRN2"),
            OperationType::SQSHRNB => write!(f, "SQSHRNB"),
            OperationType::SQSHRNT => write!(f, "SQSHRNT"),
            OperationType::SQSHRUN => write!(f, "SQSHRUN"),
            OperationType::SQSHRUN2 => write!(f, "SQSHRUN2"),
            OperationType::SQSHRUNB => write!(f, "SQSHRUNB"),
            OperationType::SQSHRUNT => write!(f, "SQSHRUNT"),
            OperationType::SQSUB => write!(f, "SQSUB"),
            OperationType::SQSUBR => write!(f, "SQSUBR"),
            OperationType::SQXTN => write!(f, "SQXTN"),
            OperationType::SQXTN2 => write!(f, "SQXTN2"),
            OperationType::SQXTNB => write!(f, "SQXTNB"),
            OperationType::SQXTNT => write!(f, "SQXTNT"),
            OperationType::SQXTUN => write!(f, "SQXTUN"),
            OperationType::SQXTUN2 => write!(f, "SQXTUN2"),
            OperationType::SQXTUNB => write!(f, "SQXTUNB"),
            OperationType::SQXTUNT => write!(f, "SQXTUNT"),
            OperationType::SRHADD => write!(f, "SRHADD"),
            OperationType::SRI => write!(f, "SRI"),
            OperationType::SRSHL => write!(f, "SRSHL"),
            OperationType::SRSHLR => write!(f, "SRSHLR"),
            OperationType::SRSHR => write!(f, "SRSHR"),
            OperationType::SRSRA => write!(f, "SRSRA"),
            OperationType::SSBB => write!(f, "SSBB"),
            OperationType::SSHL => write!(f, "SSHL"),
            OperationType::SSHLL => write!(f, "SSHLL"),
            OperationType::SSHLL2 => write!(f, "SSHLL2"),
            OperationType::SSHLLB => write!(f, "SSHLLB"),
            OperationType::SSHLLT => write!(f, "SSHLLT"),
            OperationType::SSHR => write!(f, "SSHR"),
            OperationType::SSRA => write!(f, "SSRA"),
            OperationType::SSUBL => write!(f, "SSUBL"),
            OperationType::SSUBL2 => write!(f, "SSUBL2"),
            OperationType::SSUBLB => write!(f, "SSUBLB"),
            OperationType::SSUBLBT => write!(f, "SSUBLBT"),
            OperationType::SSUBLT => write!(f, "SSUBLT"),
            OperationType::SSUBLTB => write!(f, "SSUBLTB"),
            OperationType::SSUBW => write!(f, "SSUBW"),
            OperationType::SSUBW2 => write!(f, "SSUBW2"),
            OperationType::SSUBWB => write!(f, "SSUBWB"),
            OperationType::SSUBWT => write!(f, "SSUBWT"),
            OperationType::ST1 => write!(f, "ST1"),
            OperationType::ST1B => write!(f, "ST1B"),
            OperationType::ST1D => write!(f, "ST1D"),
            OperationType::ST1H => write!(f, "ST1H"),
            OperationType::ST1Q => write!(f, "ST1Q"),
            OperationType::ST1W => write!(f, "ST1W"),
            OperationType::ST2 => write!(f, "ST2"),
            OperationType::ST2B => write!(f, "ST2B"),
            OperationType::ST2D => write!(f, "ST2D"),
            OperationType::ST2G => write!(f, "ST2G"),
            OperationType::ST2H => write!(f, "ST2H"),
            OperationType::ST2W => write!(f, "ST2W"),
            OperationType::ST3 => write!(f, "ST3"),
            OperationType::ST3B => write!(f, "ST3B"),
            OperationType::ST3D => write!(f, "ST3D"),
            OperationType::ST3H => write!(f, "ST3H"),
            OperationType::ST3W => write!(f, "ST3W"),
            OperationType::ST4 => write!(f, "ST4"),
            OperationType::ST4B => write!(f, "ST4B"),
            OperationType::ST4D => write!(f, "ST4D"),
            OperationType::ST4H => write!(f, "ST4H"),
            OperationType::ST4W => write!(f, "ST4W"),
            OperationType::ST64B => write!(f, "ST64B"),
            OperationType::ST64BV => write!(f, "ST64BV"),
            OperationType::ST64BV0 => write!(f, "ST64BV0"),
            OperationType::STADD => write!(f, "STADD"),
            OperationType::STADDB => write!(f, "STADDB"),
            OperationType::STADDH => write!(f, "STADDH"),
            OperationType::STADDL => write!(f, "STADDL"),
            OperationType::STADDLB => write!(f, "STADDLB"),
            OperationType::STADDLH => write!(f, "STADDLH"),
            OperationType::STCLR => write!(f, "STCLR"),
            OperationType::STCLRB => write!(f, "STCLRB"),
            OperationType::STCLRH => write!(f, "STCLRH"),
            OperationType::STCLRL => write!(f, "STCLRL"),
            OperationType::STCLRLB => write!(f, "STCLRLB"),
            OperationType::STCLRLH => write!(f, "STCLRLH"),
            OperationType::STEOR => write!(f, "STEOR"),
            OperationType::STEORB => write!(f, "STEORB"),
            OperationType::STEORH => write!(f, "STEORH"),
            OperationType::STEORL => write!(f, "STEORL"),
            OperationType::STEORLB => write!(f, "STEORLB"),
            OperationType::STEORLH => write!(f, "STEORLH"),
            OperationType::STG => write!(f, "STG"),
            OperationType::STGM => write!(f, "STGM"),
            OperationType::STGP => write!(f, "STGP"),
            OperationType::STLLR => write!(f, "STLLR"),
            OperationType::STLLRB => write!(f, "STLLRB"),
            OperationType::STLLRH => write!(f, "STLLRH"),
            OperationType::STLR => write!(f, "STLR"),
            OperationType::STLRB => write!(f, "STLRB"),
            OperationType::STLRH => write!(f, "STLRH"),
            OperationType::STLUR => write!(f, "STLUR"),
            OperationType::STLURB => write!(f, "STLURB"),
            OperationType::STLURH => write!(f, "STLURH"),
            OperationType::STLXP => write!(f, "STLXP"),
            OperationType::STLXR => write!(f, "STLXR"),
            OperationType::STLXRB => write!(f, "STLXRB"),
            OperationType::STLXRH => write!(f, "STLXRH"),
            OperationType::STNP => write!(f, "STNP"),
            OperationType::STNT1B => write!(f, "STNT1B"),
            OperationType::STNT1D => write!(f, "STNT1D"),
            OperationType::STNT1H => write!(f, "STNT1H"),
            OperationType::STNT1W => write!(f, "STNT1W"),
            OperationType::STP => write!(f, "STP"),
            OperationType::STR => write!(f, "STR"),
            OperationType::STRB => write!(f, "STRB"),
            OperationType::STRH => write!(f, "STRH"),
            OperationType::STSET => write!(f, "STSET"),
            OperationType::STSETB => write!(f, "STSETB"),
            OperationType::STSETH => write!(f, "STSETH"),
            OperationType::STSETL => write!(f, "STSETL"),
            OperationType::STSETLB => write!(f, "STSETLB"),
            OperationType::STSETLH => write!(f, "STSETLH"),
            OperationType::STSMAX => write!(f, "STSMAX"),
            OperationType::STSMAXB => write!(f, "STSMAXB"),
            OperationType::STSMAXH => write!(f, "STSMAXH"),
            OperationType::STSMAXL => write!(f, "STSMAXL"),
            OperationType::STSMAXLB => write!(f, "STSMAXLB"),
            OperationType::STSMAXLH => write!(f, "STSMAXLH"),
            OperationType::STSMIN => write!(f, "STSMIN"),
            OperationType::STSMINB => write!(f, "STSMINB"),
            OperationType::STSMINH => write!(f, "STSMINH"),
            OperationType::STSMINL => write!(f, "STSMINL"),
            OperationType::STSMINLB => write!(f, "STSMINLB"),
            OperationType::STSMINLH => write!(f, "STSMINLH"),
            OperationType::STTR => write!(f, "STTR"),
            OperationType::STTRB => write!(f, "STTRB"),
            OperationType::STTRH => write!(f, "STTRH"),
            OperationType::STUMAX => write!(f, "STUMAX"),
            OperationType::STUMAXB => write!(f, "STUMAXB"),
            OperationType::STUMAXH => write!(f, "STUMAXH"),
            OperationType::STUMAXL => write!(f, "STUMAXL"),
            OperationType::STUMAXLB => write!(f, "STUMAXLB"),
            OperationType::STUMAXLH => write!(f, "STUMAXLH"),
            OperationType::STUMIN => write!(f, "STUMIN"),
            OperationType::STUMINB => write!(f, "STUMINB"),
            OperationType::STUMINH => write!(f, "STUMINH"),
            OperationType::STUMINL => write!(f, "STUMINL"),
            OperationType::STUMINLB => write!(f, "STUMINLB"),
            OperationType::STUMINLH => write!(f, "STUMINLH"),
            OperationType::STUR => write!(f, "STUR"),
            OperationType::STURB => write!(f, "STURB"),
            OperationType::STURH => write!(f, "STURH"),
            OperationType::STXP => write!(f, "STXP"),
            OperationType::STXR => write!(f, "STXR"),
            OperationType::STXRB => write!(f, "STXRB"),
            OperationType::STXRH => write!(f, "STXRH"),
            OperationType::STZ2G => write!(f, "STZ2G"),
            OperationType::STZG => write!(f, "STZG"),
            OperationType::STZGM => write!(f, "STZGM"),
            OperationType::SUB => write!(f, "SUB"),
            OperationType::SUBG => write!(f, "SUBG"),
            OperationType::SUBHN => write!(f, "SUBHN"),
            OperationType::SUBHN2 => write!(f, "SUBHN2"),
            OperationType::SUBHNB => write!(f, "SUBHNB"),
            OperationType::SUBHNT => write!(f, "SUBHNT"),
            OperationType::SUBP => write!(f, "SUBP"),
            OperationType::SUBPS => write!(f, "SUBPS"),
            OperationType::SUBR => write!(f, "SUBR"),
            OperationType::SUBS => write!(f, "SUBS"),
            OperationType::SUDOT => write!(f, "SUDOT"),
            OperationType::SUMOPA => write!(f, "SUMOPA"),
            OperationType::SUMOPS => write!(f, "SUMOPS"),
            OperationType::SUNPKHI => write!(f, "SUNPKHI"),
            OperationType::SUNPKLO => write!(f, "SUNPKLO"),
            OperationType::SUQADD => write!(f, "SUQADD"),
            OperationType::SVC => write!(f, "SVC"),
            OperationType::SWP => write!(f, "SWP"),
            OperationType::SWPA => write!(f, "SWPA"),
            OperationType::SWPAB => write!(f, "SWPAB"),
            OperationType::SWPAH => write!(f, "SWPAH"),
            OperationType::SWPAL => write!(f, "SWPAL"),
            OperationType::SWPALB => write!(f, "SWPALB"),
            OperationType::SWPALH => write!(f, "SWPALH"),
            OperationType::SWPB => write!(f, "SWPB"),
            OperationType::SWPH => write!(f, "SWPH"),
            OperationType::SWPL => write!(f, "SWPL"),
            OperationType::SWPLB => write!(f, "SWPLB"),
            OperationType::SWPLH => write!(f, "SWPLH"),
            OperationType::SXTB => write!(f, "SXTB"),
            OperationType::SXTH => write!(f, "SXTH"),
            OperationType::SXTL => write!(f, "SXTL"),
            OperationType::SXTL2 => write!(f, "SXTL2"),
            OperationType::SXTW => write!(f, "SXTW"),
            OperationType::SYS => write!(f, "SYS"),
            OperationType::SYSL => write!(f, "SYSL"),
            OperationType::TBL => write!(f, "TBL"),
            OperationType::TBNZ => write!(f, "TBNZ"),
            OperationType::TBX => write!(f, "TBX"),
            OperationType::TBZ => write!(f, "TBZ"),
            OperationType::TCANCEL => write!(f, "TCANCEL"),
            OperationType::TCOMMIT => write!(f, "TCOMMIT"),
            OperationType::TLBI => write!(f, "TLBI"),
            OperationType::TRN1 => write!(f, "TRN1"),
            OperationType::TRN2 => write!(f, "TRN2"),
            OperationType::TSB => write!(f, "TSB"),
            OperationType::TST => write!(f, "TST"),
            OperationType::TSTART => write!(f, "TSTART"),
            OperationType::TTEST => write!(f, "TTEST"),
            OperationType::UABA => write!(f, "UABA"),
            OperationType::UABAL => write!(f, "UABAL"),
            OperationType::UABAL2 => write!(f, "UABAL2"),
            OperationType::UABALB => write!(f, "UABALB"),
            OperationType::UABALT => write!(f, "UABALT"),
            OperationType::UABD => write!(f, "UABD"),
            OperationType::UABDL => write!(f, "UABDL"),
            OperationType::UABDL2 => write!(f, "UABDL2"),
            OperationType::UABDLB => write!(f, "UABDLB"),
            OperationType::UABDLT => write!(f, "UABDLT"),
            OperationType::UADALP => write!(f, "UADALP"),
            OperationType::UADDL => write!(f, "UADDL"),
            OperationType::UADDL2 => write!(f, "UADDL2"),
            OperationType::UADDLB => write!(f, "UADDLB"),
            OperationType::UADDLP => write!(f, "UADDLP"),
            OperationType::UADDLT => write!(f, "UADDLT"),
            OperationType::UADDLV => write!(f, "UADDLV"),
            OperationType::UADDV => write!(f, "UADDV"),
            OperationType::UADDW => write!(f, "UADDW"),
            OperationType::UADDW2 => write!(f, "UADDW2"),
            OperationType::UADDWB => write!(f, "UADDWB"),
            OperationType::UADDWT => write!(f, "UADDWT"),
            OperationType::UBFIZ => write!(f, "UBFIZ"),
            OperationType::UBFM => write!(f, "UBFM"),
            OperationType::UBFX => write!(f, "UBFX"),
            OperationType::UCLAMP => write!(f, "UCLAMP"),
            OperationType::UCVTF => write!(f, "UCVTF"),
            OperationType::UDF => write!(f, "UDF"),
            OperationType::UDIV => write!(f, "UDIV"),
            OperationType::UDIVR => write!(f, "UDIVR"),
            OperationType::UDOT => write!(f, "UDOT"),
            OperationType::UHADD => write!(f, "UHADD"),
            OperationType::UHSUB => write!(f, "UHSUB"),
            OperationType::UHSUBR => write!(f, "UHSUBR"),
            OperationType::UMADDL => write!(f, "UMADDL"),
            OperationType::UMAX => write!(f, "UMAX"),
            OperationType::UMAXP => write!(f, "UMAXP"),
            OperationType::UMAXV => write!(f, "UMAXV"),
            OperationType::UMIN => write!(f, "UMIN"),
            OperationType::UMINP => write!(f, "UMINP"),
            OperationType::UMINV => write!(f, "UMINV"),
            OperationType::UMLAL => write!(f, "UMLAL"),
            OperationType::UMLAL2 => write!(f, "UMLAL2"),
            OperationType::UMLALB => write!(f, "UMLALB"),
            OperationType::UMLALT => write!(f, "UMLALT"),
            OperationType::UMLSL => write!(f, "UMLSL"),
            OperationType::UMLSL2 => write!(f, "UMLSL2"),
            OperationType::UMLSLB => write!(f, "UMLSLB"),
            OperationType::UMLSLT => write!(f, "UMLSLT"),
            OperationType::UMMLA => write!(f, "UMMLA"),
            OperationType::UMNEGL => write!(f, "UMNEGL"),
            OperationType::UMOPA => write!(f, "UMOPA"),
            OperationType::UMOPS => write!(f, "UMOPS"),
            OperationType::UMOV => write!(f, "UMOV"),
            OperationType::UMSUBL => write!(f, "UMSUBL"),
            OperationType::UMULH => write!(f, "UMULH"),
            OperationType::UMULL => write!(f, "UMULL"),
            OperationType::UMULL2 => write!(f, "UMULL2"),
            OperationType::UMULLB => write!(f, "UMULLB"),
            OperationType::UMULLT => write!(f, "UMULLT"),
            OperationType::UQADD => write!(f, "UQADD"),
            OperationType::UQDECB => write!(f, "UQDECB"),
            OperationType::UQDECD => write!(f, "UQDECD"),
            OperationType::UQDECH => write!(f, "UQDECH"),
            OperationType::UQDECP => write!(f, "UQDECP"),
            OperationType::UQDECW => write!(f, "UQDECW"),
            OperationType::UQINCB => write!(f, "UQINCB"),
            OperationType::UQINCD => write!(f, "UQINCD"),
            OperationType::UQINCH => write!(f, "UQINCH"),
            OperationType::UQINCP => write!(f, "UQINCP"),
            OperationType::UQINCW => write!(f, "UQINCW"),
            OperationType::UQRSHL => write!(f, "UQRSHL"),
            OperationType::UQRSHLR => write!(f, "UQRSHLR"),
            OperationType::UQRSHRN => write!(f, "UQRSHRN"),
            OperationType::UQRSHRN2 => write!(f, "UQRSHRN2"),
            OperationType::UQRSHRNB => write!(f, "UQRSHRNB"),
            OperationType::UQRSHRNT => write!(f, "UQRSHRNT"),
            OperationType::UQSHL => write!(f, "UQSHL"),
            OperationType::UQSHLR => write!(f, "UQSHLR"),
            OperationType::UQSHRN => write!(f, "UQSHRN"),
            OperationType::UQSHRN2 => write!(f, "UQSHRN2"),
            OperationType::UQSHRNB => write!(f, "UQSHRNB"),
            OperationType::UQSHRNT => write!(f, "UQSHRNT"),
            OperationType::UQSUB => write!(f, "UQSUB"),
            OperationType::UQSUBR => write!(f, "UQSUBR"),
            OperationType::UQXTN => write!(f, "UQXTN"),
            OperationType::UQXTN2 => write!(f, "UQXTN2"),
            OperationType::UQXTNB => write!(f, "UQXTNB"),
            OperationType::UQXTNT => write!(f, "UQXTNT"),
            OperationType::URECPE => write!(f, "URECPE"),
            OperationType::URHADD => write!(f, "URHADD"),
            OperationType::URSHL => write!(f, "URSHL"),
            OperationType::URSHLR => write!(f, "URSHLR"),
            OperationType::URSHR => write!(f, "URSHR"),
            OperationType::URSQRTE => write!(f, "URSQRTE"),
            OperationType::URSRA => write!(f, "URSRA"),
            OperationType::USDOT => write!(f, "USDOT"),
            OperationType::USHL => write!(f, "USHL"),
            OperationType::USHLL => write!(f, "USHLL"),
            OperationType::USHLL2 => write!(f, "USHLL2"),
            OperationType::USHLLB => write!(f, "USHLLB"),
            OperationType::USHLLT => write!(f, "USHLLT"),
            OperationType::USHR => write!(f, "USHR"),
            OperationType::USMMLA => write!(f, "USMMLA"),
            OperationType::USMOPA => write!(f, "USMOPA"),
            OperationType::USMOPS => write!(f, "USMOPS"),
            OperationType::USQADD => write!(f, "USQADD"),
            OperationType::USRA => write!(f, "USRA"),
            OperationType::USUBL => write!(f, "USUBL"),
            OperationType::USUBL2 => write!(f, "USUBL2"),
            OperationType::USUBLB => write!(f, "USUBLB"),
            OperationType::USUBLT => write!(f, "USUBLT"),
            OperationType::USUBW => write!(f, "USUBW"),
            OperationType::USUBW2 => write!(f, "USUBW2"),
            OperationType::USUBWB => write!(f, "USUBWB"),
            OperationType::USUBWT => write!(f, "USUBWT"),
            OperationType::UUNPKHI => write!(f, "UUNPKHI"),
            OperationType::UUNPKLO => write!(f, "UUNPKLO"),
            OperationType::UXTB => write!(f, "UXTB"),
            OperationType::UXTH => write!(f, "UXTH"),
            OperationType::UXTL => write!(f, "UXTL"),
            OperationType::UXTL2 => write!(f, "UXTL2"),
            OperationType::UXTW => write!(f, "UXTW"),
            OperationType::UZP1 => write!(f, "UZP1"),
            OperationType::UZP2 => write!(f, "UZP2"),
            OperationType::WFE => write!(f, "WFE"),
            OperationType::WFET => write!(f, "WFET"),
            OperationType::WFI => write!(f, "WFI"),
            OperationType::WFIT => write!(f, "WFIT"),
            OperationType::WHILEGE => write!(f, "WHILEGE"),
            OperationType::WHILEGT => write!(f, "WHILEGT"),
            OperationType::WHILEHI => write!(f, "WHILEHI"),
            OperationType::WHILEHS => write!(f, "WHILEHS"),
            OperationType::WHILELE => write!(f, "WHILELE"),
            OperationType::WHILELO => write!(f, "WHILELO"),
            OperationType::WHILELS => write!(f, "WHILELS"),
            OperationType::WHILELT => write!(f, "WHILELT"),
            OperationType::WHILERW => write!(f, "WHILERW"),
            OperationType::WHILEWR => write!(f, "WHILEWR"),
            OperationType::WRFFR => write!(f, "WRFFR"),
            OperationType::XAFLAG => write!(f, "XAFLAG"),
            OperationType::XAR => write!(f, "XAR"),
            OperationType::XPACD => write!(f, "XPACD"),
            OperationType::XPACI => write!(f, "XPACI"),
            OperationType::XPACLRI => write!(f, "XPACLRI"),
            OperationType::XTN => write!(f, "XTN"),
            OperationType::XTN2 => write!(f, "XTN2"),
            OperationType::YIELD => write!(f, "YIELD"),
            OperationType::ZERO => write!(f, "ZERO"),
            OperationType::ZIP1 => write!(f, "ZIP1"),
            OperationType::ZIP2 => write!(f, "ZIP2"),
        }
    }
}

impl From<usize> for OperationType {
    fn from(value: usize) -> Self {
        match value {
            0x0 => Self::ERROR,
            0x1 => Self::ABS,
            0x2 => Self::ADC,
            0x3 => Self::ADCLB,
            0x4 => Self::ADCLT,
            0x5 => Self::ADCS,
            0x6 => Self::ADD,
            0x7 => Self::ADDG,
            0x8 => Self::ADDHA,
            0x9 => Self::ADDHN,
            0xa => Self::ADDHN2,
            0xb => Self::ADDHNB,
            0xc => Self::ADDHNT,
            0xd => Self::ADDP,
            0xe => Self::ADDPL,
            0xf => Self::ADDS,
            0x10 => Self::ADDV,
            0x11 => Self::ADDVA,
            0x12 => Self::ADDVL,
            0x13 => Self::ADR,
            0x14 => Self::ADRP,
            0x15 => Self::AESD,
            0x16 => Self::AESE,
            0x17 => Self::AESIMC,
            0x18 => Self::AESMC,
            0x19 => Self::AND,
            0x1a => Self::ANDS,
            0x1b => Self::ANDV,
            0x1c => Self::ASR,
            0x1d => Self::ASRD,
            0x1e => Self::ASRR,
            0x1f => Self::ASRV,
            0x20 => Self::AT,
            0x21 => Self::AUTDA,
            0x22 => Self::AUTDB,
            0x23 => Self::AUTDZA,
            0x24 => Self::AUTDZB,
            0x25 => Self::AUTIA,
            0x26 => Self::AUTIA1716,
            0x27 => Self::AUTIASP,
            0x28 => Self::AUTIAZ,
            0x29 => Self::AUTIB,
            0x2a => Self::AUTIB1716,
            0x2b => Self::AUTIBSP,
            0x2c => Self::AUTIBZ,
            0x2d => Self::AUTIZA,
            0x2e => Self::AUTIZB,
            0x2f => Self::AXFLAG,
            0x30 => Self::B,
            0x31 => Self::BCAX,
            0x32 => Self::BDEP,
            0x33 => Self::BEXT,
            0x34 => Self::BFC,
            0x35 => Self::BFCVT,
            0x36 => Self::BFCVTN,
            0x37 => Self::BFCVTN2,
            0x38 => Self::BFCVTNT,
            0x39 => Self::BFDOT,
            0x3a => Self::BFI,
            0x3b => Self::BFM,
            0x3c => Self::BFMLAL,
            0x3d => Self::BFMLALB,
            0x3e => Self::BFMLALT,
            0x3f => Self::BFMMLA,
            0x40 => Self::BFMOPA,
            0x41 => Self::BFMOPS,
            0x42 => Self::BFXIL,
            0x43 => Self::BGRP,
            0x44 => Self::BIC,
            0x45 => Self::BICS,
            0x46 => Self::BIF,
            0x47 => Self::BIT,
            0x48 => Self::BL,
            0x49 => Self::BLR,
            0x4a => Self::BLRAA,
            0x4b => Self::BLRAAZ,
            0x4c => Self::BLRAB,
            0x4d => Self::BLRABZ,
            0x4e => Self::BR,
            0x4f => Self::BRAA,
            0x50 => Self::BRAAZ,
            0x51 => Self::BRAB,
            0x52 => Self::BRABZ,
            0x53 => Self::BRK,
            0x54 => Self::BRKA,
            0x55 => Self::BRKAS,
            0x56 => Self::BRKB,
            0x57 => Self::BRKBS,
            0x58 => Self::BRKN,
            0x59 => Self::BRKNS,
            0x5a => Self::BRKPA,
            0x5b => Self::BRKPAS,
            0x5c => Self::BRKPB,
            0x5d => Self::BRKPBS,
            0x5e => Self::BSL,
            0x5f => Self::BSL1N,
            0x60 => Self::BSL2N,
            0x61 => Self::BTI,
            0x62 => Self::B_AL,
            0x63 => Self::B_CC,
            0x64 => Self::B_CS,
            0x65 => Self::B_EQ,
            0x66 => Self::B_GE,
            0x67 => Self::B_GT,
            0x68 => Self::B_HI,
            0x69 => Self::B_LE,
            0x6a => Self::B_LS,
            0x6b => Self::B_LT,
            0x6c => Self::B_MI,
            0x6d => Self::B_NE,
            0x6e => Self::B_NV,
            0x6f => Self::B_PL,
            0x70 => Self::B_VC,
            0x71 => Self::B_VS,
            0x72 => Self::CADD,
            0x73 => Self::CAS,
            0x74 => Self::CASA,
            0x75 => Self::CASAB,
            0x76 => Self::CASAH,
            0x77 => Self::CASAL,
            0x78 => Self::CASALB,
            0x79 => Self::CASALH,
            0x7a => Self::CASB,
            0x7b => Self::CASH,
            0x7c => Self::CASL,
            0x7d => Self::CASLB,
            0x7e => Self::CASLH,
            0x7f => Self::CASP,
            0x80 => Self::CASPA,
            0x81 => Self::CASPAL,
            0x82 => Self::CASPL,
            0x83 => Self::CBNZ,
            0x84 => Self::CBZ,
            0x85 => Self::CCMN,
            0x86 => Self::CCMP,
            0x87 => Self::CDOT,
            0x88 => Self::CFINV,
            0x89 => Self::CFP,
            0x8a => Self::CINC,
            0x8b => Self::CINV,
            0x8c => Self::CLASTA,
            0x8d => Self::CLASTB,
            0x8e => Self::CLREX,
            0x8f => Self::CLS,
            0x90 => Self::CLZ,
            0x91 => Self::CMEQ,
            0x92 => Self::CMGE,
            0x93 => Self::CMGT,
            0x94 => Self::CMHI,
            0x95 => Self::CMHS,
            0x96 => Self::CMLA,
            0x97 => Self::CMLE,
            0x98 => Self::CMLT,
            0x99 => Self::CMN,
            0x9a => Self::CMP,
            0x9b => Self::CMPEQ,
            0x9c => Self::CMPGE,
            0x9d => Self::CMPGT,
            0x9e => Self::CMPHI,
            0x9f => Self::CMPHS,
            0xa0 => Self::CMPLE,
            0xa1 => Self::CMPLO,
            0xa2 => Self::CMPLS,
            0xa3 => Self::CMPLT,
            0xa4 => Self::CMPNE,
            0xa5 => Self::CMPP,
            0xa6 => Self::CMTST,
            0xa7 => Self::CNEG,
            0xa8 => Self::CNOT,
            0xa9 => Self::CNT,
            0xaa => Self::CNTB,
            0xab => Self::CNTD,
            0xac => Self::CNTH,
            0xad => Self::CNTP,
            0xae => Self::CNTW,
            0xaf => Self::COMPACT,
            0xb0 => Self::CPP,
            0xb1 => Self::CPY,
            0xb2 => Self::CRC32B,
            0xb3 => Self::CRC32CB,
            0xb4 => Self::CRC32CH,
            0xb5 => Self::CRC32CW,
            0xb6 => Self::CRC32CX,
            0xb7 => Self::CRC32H,
            0xb8 => Self::CRC32W,
            0xb9 => Self::CRC32X,
            0xba => Self::CSDB,
            0xbb => Self::CSEL,
            0xbc => Self::CSET,
            0xbd => Self::CSETM,
            0xbe => Self::CSINC,
            0xbf => Self::CSINV,
            0xc0 => Self::CSNEG,
            0xc1 => Self::CTERMEQ,
            0xc2 => Self::CTERMNE,
            0xc3 => Self::DC,
            0xc4 => Self::DCPS1,
            0xc5 => Self::DCPS2,
            0xc6 => Self::DCPS3,
            0xc7 => Self::DECB,
            0xc8 => Self::DECD,
            0xc9 => Self::DECH,
            0xca => Self::DECP,
            0xcb => Self::DECW,
            0xcc => Self::DGH,
            0xcd => Self::DMB,
            0xce => Self::DRPS,
            0xcf => Self::DSB,
            0xd0 => Self::DUP,
            0xd1 => Self::DUPM,
            0xd2 => Self::DVP,
            0xd3 => Self::EON,
            0xd4 => Self::EOR,
            0xd5 => Self::EOR3,
            0xd6 => Self::EORBT,
            0xd7 => Self::EORS,
            0xd8 => Self::EORTB,
            0xd9 => Self::EORV,
            0xda => Self::ERET,
            0xdb => Self::ERETAA,
            0xdc => Self::ERETAB,
            0xdd => Self::ESB,
            0xde => Self::EXT,
            0xdf => Self::EXTR,
            0xe0 => Self::FABD,
            0xe1 => Self::FABS,
            0xe2 => Self::FACGE,
            0xe3 => Self::FACGT,
            0xe4 => Self::FACLE,
            0xe5 => Self::FACLT,
            0xe6 => Self::FADD,
            0xe7 => Self::FADDA,
            0xe8 => Self::FADDP,
            0xe9 => Self::FADDV,
            0xea => Self::FCADD,
            0xeb => Self::FCCMP,
            0xec => Self::FCCMPE,
            0xed => Self::FCMEQ,
            0xee => Self::FCMGE,
            0xef => Self::FCMGT,
            0xf0 => Self::FCMLA,
            0xf1 => Self::FCMLE,
            0xf2 => Self::FCMLT,
            0xf3 => Self::FCMNE,
            0xf4 => Self::FCMP,
            0xf5 => Self::FCMPE,
            0xf6 => Self::FCMUO,
            0xf7 => Self::FCPY,
            0xf8 => Self::FCSEL,
            0xf9 => Self::FCVT,
            0xfa => Self::FCVTAS,
            0xfb => Self::FCVTAU,
            0xfc => Self::FCVTL,
            0xfd => Self::FCVTL2,
            0xfe => Self::FCVTLT,
            0xff => Self::FCVTMS,
            0x100 => Self::FCVTMU,
            0x101 => Self::FCVTN,
            0x102 => Self::FCVTN2,
            0x103 => Self::FCVTNS,
            0x104 => Self::FCVTNT,
            0x105 => Self::FCVTNU,
            0x106 => Self::FCVTPS,
            0x107 => Self::FCVTPU,
            0x108 => Self::FCVTX,
            0x109 => Self::FCVTXN,
            0x10a => Self::FCVTXN2,
            0x10b => Self::FCVTXNT,
            0x10c => Self::FCVTZS,
            0x10d => Self::FCVTZU,
            0x10e => Self::FDIV,
            0x10f => Self::FDIVR,
            0x110 => Self::FDUP,
            0x111 => Self::FEXPA,
            0x112 => Self::FJCVTZS,
            0x113 => Self::FLOGB,
            0x114 => Self::FMAD,
            0x115 => Self::FMADD,
            0x116 => Self::FMAX,
            0x117 => Self::FMAXNM,
            0x118 => Self::FMAXNMP,
            0x119 => Self::FMAXNMV,
            0x11a => Self::FMAXP,
            0x11b => Self::FMAXV,
            0x11c => Self::FMIN,
            0x11d => Self::FMINNM,
            0x11e => Self::FMINNMP,
            0x11f => Self::FMINNMV,
            0x120 => Self::FMINP,
            0x121 => Self::FMINV,
            0x122 => Self::FMLA,
            0x123 => Self::FMLAL,
            0x124 => Self::FMLAL2,
            0x125 => Self::FMLALB,
            0x126 => Self::FMLALT,
            0x127 => Self::FMLS,
            0x128 => Self::FMLSL,
            0x129 => Self::FMLSL2,
            0x12a => Self::FMLSLB,
            0x12b => Self::FMLSLT,
            0x12c => Self::FMMLA,
            0x12d => Self::FMOPA,
            0x12e => Self::FMOPS,
            0x12f => Self::FMOV,
            0x130 => Self::FMSB,
            0x131 => Self::FMSUB,
            0x132 => Self::FMUL,
            0x133 => Self::FMULX,
            0x134 => Self::FNEG,
            0x135 => Self::FNMAD,
            0x136 => Self::FNMADD,
            0x137 => Self::FNMLA,
            0x138 => Self::FNMLS,
            0x139 => Self::FNMSB,
            0x13a => Self::FNMSUB,
            0x13b => Self::FNMUL,
            0x13c => Self::FRECPE,
            0x13d => Self::FRECPS,
            0x13e => Self::FRECPX,
            0x13f => Self::FRINT32X,
            0x140 => Self::FRINT32Z,
            0x141 => Self::FRINT64X,
            0x142 => Self::FRINT64Z,
            0x143 => Self::FRINTA,
            0x144 => Self::FRINTI,
            0x145 => Self::FRINTM,
            0x146 => Self::FRINTN,
            0x147 => Self::FRINTP,
            0x148 => Self::FRINTX,
            0x149 => Self::FRINTZ,
            0x14a => Self::FRSQRTE,
            0x14b => Self::FRSQRTS,
            0x14c => Self::FSCALE,
            0x14d => Self::FSQRT,
            0x14e => Self::FSUB,
            0x14f => Self::FSUBR,
            0x150 => Self::FTMAD,
            0x151 => Self::FTSMUL,
            0x152 => Self::FTSSEL,
            0x153 => Self::GMI,
            0x154 => Self::HINT,
            0x155 => Self::HISTCNT,
            0x156 => Self::HISTSEG,
            0x157 => Self::HLT,
            0x158 => Self::HVC,
            0x159 => Self::IC,
            0x15a => Self::INCB,
            0x15b => Self::INCD,
            0x15c => Self::INCH,
            0x15d => Self::INCP,
            0x15e => Self::INCW,
            0x15f => Self::INDEX,
            0x160 => Self::INS,
            0x161 => Self::INSR,
            0x162 => Self::IRG,
            0x163 => Self::ISB,
            0x164 => Self::LASTA,
            0x165 => Self::LASTB,
            0x166 => Self::LD1,
            0x167 => Self::LD1B,
            0x168 => Self::LD1D,
            0x169 => Self::LD1H,
            0x16a => Self::LD1Q,
            0x16b => Self::LD1R,
            0x16c => Self::LD1RB,
            0x16d => Self::LD1RD,
            0x16e => Self::LD1RH,
            0x16f => Self::LD1ROB,
            0x170 => Self::LD1ROD,
            0x171 => Self::LD1ROH,
            0x172 => Self::LD1ROW,
            0x173 => Self::LD1RQB,
            0x174 => Self::LD1RQD,
            0x175 => Self::LD1RQH,
            0x176 => Self::LD1RQW,
            0x177 => Self::LD1RSB,
            0x178 => Self::LD1RSH,
            0x179 => Self::LD1RSW,
            0x17a => Self::LD1RW,
            0x17b => Self::LD1SB,
            0x17c => Self::LD1SH,
            0x17d => Self::LD1SW,
            0x17e => Self::LD1W,
            0x17f => Self::LD2,
            0x180 => Self::LD2B,
            0x181 => Self::LD2D,
            0x182 => Self::LD2H,
            0x183 => Self::LD2R,
            0x184 => Self::LD2W,
            0x185 => Self::LD3,
            0x186 => Self::LD3B,
            0x187 => Self::LD3D,
            0x188 => Self::LD3H,
            0x189 => Self::LD3R,
            0x18a => Self::LD3W,
            0x18b => Self::LD4,
            0x18c => Self::LD4B,
            0x18d => Self::LD4D,
            0x18e => Self::LD4H,
            0x18f => Self::LD4R,
            0x190 => Self::LD4W,
            0x191 => Self::LD64B,
            0x192 => Self::LDADD,
            0x193 => Self::LDADDA,
            0x194 => Self::LDADDAB,
            0x195 => Self::LDADDAH,
            0x196 => Self::LDADDAL,
            0x197 => Self::LDADDALB,
            0x198 => Self::LDADDALH,
            0x199 => Self::LDADDB,
            0x19a => Self::LDADDH,
            0x19b => Self::LDADDL,
            0x19c => Self::LDADDLB,
            0x19d => Self::LDADDLH,
            0x19e => Self::LDAPR,
            0x19f => Self::LDAPRB,
            0x1a0 => Self::LDAPRH,
            0x1a1 => Self::LDAPUR,
            0x1a2 => Self::LDAPURB,
            0x1a3 => Self::LDAPURH,
            0x1a4 => Self::LDAPURSB,
            0x1a5 => Self::LDAPURSH,
            0x1a6 => Self::LDAPURSW,
            0x1a7 => Self::LDAR,
            0x1a8 => Self::LDARB,
            0x1a9 => Self::LDARH,
            0x1aa => Self::LDAXP,
            0x1ab => Self::LDAXR,
            0x1ac => Self::LDAXRB,
            0x1ad => Self::LDAXRH,
            0x1ae => Self::LDCLR,
            0x1af => Self::LDCLRA,
            0x1b0 => Self::LDCLRAB,
            0x1b1 => Self::LDCLRAH,
            0x1b2 => Self::LDCLRAL,
            0x1b3 => Self::LDCLRALB,
            0x1b4 => Self::LDCLRALH,
            0x1b5 => Self::LDCLRB,
            0x1b6 => Self::LDCLRH,
            0x1b7 => Self::LDCLRL,
            0x1b8 => Self::LDCLRLB,
            0x1b9 => Self::LDCLRLH,
            0x1ba => Self::LDEOR,
            0x1bb => Self::LDEORA,
            0x1bc => Self::LDEORAB,
            0x1bd => Self::LDEORAH,
            0x1be => Self::LDEORAL,
            0x1bf => Self::LDEORALB,
            0x1c0 => Self::LDEORALH,
            0x1c1 => Self::LDEORB,
            0x1c2 => Self::LDEORH,
            0x1c3 => Self::LDEORL,
            0x1c4 => Self::LDEORLB,
            0x1c5 => Self::LDEORLH,
            0x1c6 => Self::LDFF1B,
            0x1c7 => Self::LDFF1D,
            0x1c8 => Self::LDFF1H,
            0x1c9 => Self::LDFF1SB,
            0x1ca => Self::LDFF1SH,
            0x1cb => Self::LDFF1SW,
            0x1cc => Self::LDFF1W,
            0x1cd => Self::LDG,
            0x1ce => Self::LDGM,
            0x1cf => Self::LDLAR,
            0x1d0 => Self::LDLARB,
            0x1d1 => Self::LDLARH,
            0x1d2 => Self::LDNF1B,
            0x1d3 => Self::LDNF1D,
            0x1d4 => Self::LDNF1H,
            0x1d5 => Self::LDNF1SB,
            0x1d6 => Self::LDNF1SH,
            0x1d7 => Self::LDNF1SW,
            0x1d8 => Self::LDNF1W,
            0x1d9 => Self::LDNP,
            0x1da => Self::LDNT1B,
            0x1db => Self::LDNT1D,
            0x1dc => Self::LDNT1H,
            0x1dd => Self::LDNT1SB,
            0x1de => Self::LDNT1SH,
            0x1df => Self::LDNT1SW,
            0x1e0 => Self::LDNT1W,
            0x1e1 => Self::LDP,
            0x1e2 => Self::LDPSW,
            0x1e3 => Self::LDR,
            0x1e4 => Self::LDRAA,
            0x1e5 => Self::LDRAB,
            0x1e6 => Self::LDRB,
            0x1e7 => Self::LDRH,
            0x1e8 => Self::LDRSB,
            0x1e9 => Self::LDRSH,
            0x1ea => Self::LDRSW,
            0x1eb => Self::LDSET,
            0x1ec => Self::LDSETA,
            0x1ed => Self::LDSETAB,
            0x1ee => Self::LDSETAH,
            0x1ef => Self::LDSETAL,
            0x1f0 => Self::LDSETALB,
            0x1f1 => Self::LDSETALH,
            0x1f2 => Self::LDSETB,
            0x1f3 => Self::LDSETH,
            0x1f4 => Self::LDSETL,
            0x1f5 => Self::LDSETLB,
            0x1f6 => Self::LDSETLH,
            0x1f7 => Self::LDSMAX,
            0x1f8 => Self::LDSMAXA,
            0x1f9 => Self::LDSMAXAB,
            0x1fa => Self::LDSMAXAH,
            0x1fb => Self::LDSMAXAL,
            0x1fc => Self::LDSMAXALB,
            0x1fd => Self::LDSMAXALH,
            0x1fe => Self::LDSMAXB,
            0x1ff => Self::LDSMAXH,
            0x200 => Self::LDSMAXL,
            0x201 => Self::LDSMAXLB,
            0x202 => Self::LDSMAXLH,
            0x203 => Self::LDSMIN,
            0x204 => Self::LDSMINA,
            0x205 => Self::LDSMINAB,
            0x206 => Self::LDSMINAH,
            0x207 => Self::LDSMINAL,
            0x208 => Self::LDSMINALB,
            0x209 => Self::LDSMINALH,
            0x20a => Self::LDSMINB,
            0x20b => Self::LDSMINH,
            0x20c => Self::LDSMINL,
            0x20d => Self::LDSMINLB,
            0x20e => Self::LDSMINLH,
            0x20f => Self::LDTR,
            0x210 => Self::LDTRB,
            0x211 => Self::LDTRH,
            0x212 => Self::LDTRSB,
            0x213 => Self::LDTRSH,
            0x214 => Self::LDTRSW,
            0x215 => Self::LDUMAX,
            0x216 => Self::LDUMAXA,
            0x217 => Self::LDUMAXAB,
            0x218 => Self::LDUMAXAH,
            0x219 => Self::LDUMAXAL,
            0x21a => Self::LDUMAXALB,
            0x21b => Self::LDUMAXALH,
            0x21c => Self::LDUMAXB,
            0x21d => Self::LDUMAXH,
            0x21e => Self::LDUMAXL,
            0x21f => Self::LDUMAXLB,
            0x220 => Self::LDUMAXLH,
            0x221 => Self::LDUMIN,
            0x222 => Self::LDUMINA,
            0x223 => Self::LDUMINAB,
            0x224 => Self::LDUMINAH,
            0x225 => Self::LDUMINAL,
            0x226 => Self::LDUMINALB,
            0x227 => Self::LDUMINALH,
            0x228 => Self::LDUMINB,
            0x229 => Self::LDUMINH,
            0x22a => Self::LDUMINL,
            0x22b => Self::LDUMINLB,
            0x22c => Self::LDUMINLH,
            0x22d => Self::LDUR,
            0x22e => Self::LDURB,
            0x22f => Self::LDURH,
            0x230 => Self::LDURSB,
            0x231 => Self::LDURSH,
            0x232 => Self::LDURSW,
            0x233 => Self::LDXP,
            0x234 => Self::LDXR,
            0x235 => Self::LDXRB,
            0x236 => Self::LDXRH,
            0x237 => Self::LSL,
            0x238 => Self::LSLR,
            0x239 => Self::LSLV,
            0x23a => Self::LSR,
            0x23b => Self::LSRR,
            0x23c => Self::LSRV,
            0x23d => Self::MAD,
            0x23e => Self::MADD,
            0x23f => Self::MATCH,
            0x240 => Self::MLA,
            0x241 => Self::MLS,
            0x242 => Self::MNEG,
            0x243 => Self::MOV,
            0x244 => Self::MOVA,
            0x245 => Self::MOVI,
            0x246 => Self::MOVK,
            0x247 => Self::MOVN,
            0x248 => Self::MOVPRFX,
            0x249 => Self::MOVS,
            0x24a => Self::MOVZ,
            0x24b => Self::MRS,
            0x24c => Self::MSB,
            0x24d => Self::MSR,
            0x24e => Self::MSUB,
            0x24f => Self::MUL,
            0x250 => Self::MVN,
            0x251 => Self::MVNI,
            0x252 => Self::NAND,
            0x253 => Self::NANDS,
            0x254 => Self::NBSL,
            0x255 => Self::NEG,
            0x256 => Self::NEGS,
            0x257 => Self::NGC,
            0x258 => Self::NGCS,
            0x259 => Self::NMATCH,
            0x25a => Self::NOP,
            0x25b => Self::NOR,
            0x25c => Self::NORS,
            0x25d => Self::NOT,
            0x25e => Self::NOTS,
            0x25f => Self::ORN,
            0x260 => Self::ORNS,
            0x261 => Self::ORR,
            0x262 => Self::ORRS,
            0x263 => Self::ORV,
            0x264 => Self::PACDA,
            0x265 => Self::PACDB,
            0x266 => Self::PACDZA,
            0x267 => Self::PACDZB,
            0x268 => Self::PACGA,
            0x269 => Self::PACIA,
            0x26a => Self::PACIA1716,
            0x26b => Self::PACIASP,
            0x26c => Self::PACIAZ,
            0x26d => Self::PACIB,
            0x26e => Self::PACIB1716,
            0x26f => Self::PACIBSP,
            0x270 => Self::PACIBZ,
            0x271 => Self::PACIZA,
            0x272 => Self::PACIZB,
            0x273 => Self::PFALSE,
            0x274 => Self::PFIRST,
            0x275 => Self::PMUL,
            0x276 => Self::PMULL,
            0x277 => Self::PMULL2,
            0x278 => Self::PMULLB,
            0x279 => Self::PMULLT,
            0x27a => Self::PNEXT,
            0x27b => Self::PRFB,
            0x27c => Self::PRFD,
            0x27d => Self::PRFH,
            0x27e => Self::PRFM,
            0x27f => Self::PRFUM,
            0x280 => Self::PRFW,
            0x281 => Self::PSB,
            0x282 => Self::PSSBB,
            0x283 => Self::PTEST,
            0x284 => Self::PTRUE,
            0x285 => Self::PTRUES,
            0x286 => Self::PUNPKHI,
            0x287 => Self::PUNPKLO,
            0x288 => Self::RADDHN,
            0x289 => Self::RADDHN2,
            0x28a => Self::RADDHNB,
            0x28b => Self::RADDHNT,
            0x28c => Self::RAX1,
            0x28d => Self::RBIT,
            0x28e => Self::RDFFR,
            0x28f => Self::RDFFRS,
            0x290 => Self::RDVL,
            0x291 => Self::RET,
            0x292 => Self::RETAA,
            0x293 => Self::RETAB,
            0x294 => Self::REV,
            0x295 => Self::REV16,
            0x296 => Self::REV32,
            0x297 => Self::REV64,
            0x298 => Self::REVB,
            0x299 => Self::REVD,
            0x29a => Self::REVH,
            0x29b => Self::REVW,
            0x29c => Self::RMIF,
            0x29d => Self::ROR,
            0x29e => Self::RORV,
            0x29f => Self::RSHRN,
            0x2a0 => Self::RSHRN2,
            0x2a1 => Self::RSHRNB,
            0x2a2 => Self::RSHRNT,
            0x2a3 => Self::RSUBHN,
            0x2a4 => Self::RSUBHN2,
            0x2a5 => Self::RSUBHNB,
            0x2a6 => Self::RSUBHNT,
            0x2a7 => Self::SABA,
            0x2a8 => Self::SABAL,
            0x2a9 => Self::SABAL2,
            0x2aa => Self::SABALB,
            0x2ab => Self::SABALT,
            0x2ac => Self::SABD,
            0x2ad => Self::SABDL,
            0x2ae => Self::SABDL2,
            0x2af => Self::SABDLB,
            0x2b0 => Self::SABDLT,
            0x2b1 => Self::SADALP,
            0x2b2 => Self::SADDL,
            0x2b3 => Self::SADDL2,
            0x2b4 => Self::SADDLB,
            0x2b5 => Self::SADDLBT,
            0x2b6 => Self::SADDLP,
            0x2b7 => Self::SADDLT,
            0x2b8 => Self::SADDLV,
            0x2b9 => Self::SADDV,
            0x2ba => Self::SADDW,
            0x2bb => Self::SADDW2,
            0x2bc => Self::SADDWB,
            0x2bd => Self::SADDWT,
            0x2be => Self::SB,
            0x2bf => Self::SBC,
            0x2c0 => Self::SBCLB,
            0x2c1 => Self::SBCLT,
            0x2c2 => Self::SBCS,
            0x2c3 => Self::SBFIZ,
            0x2c4 => Self::SBFM,
            0x2c5 => Self::SBFX,
            0x2c6 => Self::SCLAMP,
            0x2c7 => Self::SCVTF,
            0x2c8 => Self::SDIV,
            0x2c9 => Self::SDIVR,
            0x2ca => Self::SDOT,
            0x2cb => Self::SEL,
            0x2cc => Self::SETF16,
            0x2cd => Self::SETF8,
            0x2ce => Self::SETFFR,
            0x2cf => Self::SEV,
            0x2d0 => Self::SEVL,
            0x2d1 => Self::SHA1C,
            0x2d2 => Self::SHA1H,
            0x2d3 => Self::SHA1M,
            0x2d4 => Self::SHA1P,
            0x2d5 => Self::SHA1SU0,
            0x2d6 => Self::SHA1SU1,
            0x2d7 => Self::SHA256H,
            0x2d8 => Self::SHA256H2,
            0x2d9 => Self::SHA256SU0,
            0x2da => Self::SHA256SU1,
            0x2db => Self::SHA512H,
            0x2dc => Self::SHA512H2,
            0x2dd => Self::SHA512SU0,
            0x2de => Self::SHA512SU1,
            0x2df => Self::SHADD,
            0x2e0 => Self::SHL,
            0x2e1 => Self::SHLL,
            0x2e2 => Self::SHLL2,
            0x2e3 => Self::SHRN,
            0x2e4 => Self::SHRN2,
            0x2e5 => Self::SHRNB,
            0x2e6 => Self::SHRNT,
            0x2e7 => Self::SHSUB,
            0x2e8 => Self::SHSUBR,
            0x2e9 => Self::SLI,
            0x2ea => Self::SM3PARTW1,
            0x2eb => Self::SM3PARTW2,
            0x2ec => Self::SM3SS1,
            0x2ed => Self::SM3TT1A,
            0x2ee => Self::SM3TT1B,
            0x2ef => Self::SM3TT2A,
            0x2f0 => Self::SM3TT2B,
            0x2f1 => Self::SM4E,
            0x2f2 => Self::SM4EKEY,
            0x2f3 => Self::SMADDL,
            0x2f4 => Self::SMAX,
            0x2f5 => Self::SMAXP,
            0x2f6 => Self::SMAXV,
            0x2f7 => Self::SMC,
            0x2f8 => Self::SMIN,
            0x2f9 => Self::SMINP,
            0x2fa => Self::SMINV,
            0x2fb => Self::SMLAL,
            0x2fc => Self::SMLAL2,
            0x2fd => Self::SMLALB,
            0x2fe => Self::SMLALT,
            0x2ff => Self::SMLSL,
            0x300 => Self::SMLSL2,
            0x301 => Self::SMLSLB,
            0x302 => Self::SMLSLT,
            0x303 => Self::SMMLA,
            0x304 => Self::SMNEGL,
            0x305 => Self::SMOPA,
            0x306 => Self::SMOPS,
            0x307 => Self::SMOV,
            0x308 => Self::SMSTART,
            0x309 => Self::SMSTOP,
            0x30a => Self::SMSUBL,
            0x30b => Self::SMULH,
            0x30c => Self::SMULL,
            0x30d => Self::SMULL2,
            0x30e => Self::SMULLB,
            0x30f => Self::SMULLT,
            0x310 => Self::SPLICE,
            0x311 => Self::SQABS,
            0x312 => Self::SQADD,
            0x313 => Self::SQCADD,
            0x314 => Self::SQDECB,
            0x315 => Self::SQDECD,
            0x316 => Self::SQDECH,
            0x317 => Self::SQDECP,
            0x318 => Self::SQDECW,
            0x319 => Self::SQDMLAL,
            0x31a => Self::SQDMLAL2,
            0x31b => Self::SQDMLALB,
            0x31c => Self::SQDMLALBT,
            0x31d => Self::SQDMLALT,
            0x31e => Self::SQDMLSL,
            0x31f => Self::SQDMLSL2,
            0x320 => Self::SQDMLSLB,
            0x321 => Self::SQDMLSLBT,
            0x322 => Self::SQDMLSLT,
            0x323 => Self::SQDMULH,
            0x324 => Self::SQDMULL,
            0x325 => Self::SQDMULL2,
            0x326 => Self::SQDMULLB,
            0x327 => Self::SQDMULLT,
            0x328 => Self::SQINCB,
            0x329 => Self::SQINCD,
            0x32a => Self::SQINCH,
            0x32b => Self::SQINCP,
            0x32c => Self::SQINCW,
            0x32d => Self::SQNEG,
            0x32e => Self::SQRDCMLAH,
            0x32f => Self::SQRDMLAH,
            0x330 => Self::SQRDMLSH,
            0x331 => Self::SQRDMULH,
            0x332 => Self::SQRSHL,
            0x333 => Self::SQRSHLR,
            0x334 => Self::SQRSHRN,
            0x335 => Self::SQRSHRN2,
            0x336 => Self::SQRSHRNB,
            0x337 => Self::SQRSHRNT,
            0x338 => Self::SQRSHRUN,
            0x339 => Self::SQRSHRUN2,
            0x33a => Self::SQRSHRUNB,
            0x33b => Self::SQRSHRUNT,
            0x33c => Self::SQSHL,
            0x33d => Self::SQSHLR,
            0x33e => Self::SQSHLU,
            0x33f => Self::SQSHRN,
            0x340 => Self::SQSHRN2,
            0x341 => Self::SQSHRNB,
            0x342 => Self::SQSHRNT,
            0x343 => Self::SQSHRUN,
            0x344 => Self::SQSHRUN2,
            0x345 => Self::SQSHRUNB,
            0x346 => Self::SQSHRUNT,
            0x347 => Self::SQSUB,
            0x348 => Self::SQSUBR,
            0x349 => Self::SQXTN,
            0x34a => Self::SQXTN2,
            0x34b => Self::SQXTNB,
            0x34c => Self::SQXTNT,
            0x34d => Self::SQXTUN,
            0x34e => Self::SQXTUN2,
            0x34f => Self::SQXTUNB,
            0x350 => Self::SQXTUNT,
            0x351 => Self::SRHADD,
            0x352 => Self::SRI,
            0x353 => Self::SRSHL,
            0x354 => Self::SRSHLR,
            0x355 => Self::SRSHR,
            0x356 => Self::SRSRA,
            0x357 => Self::SSBB,
            0x358 => Self::SSHL,
            0x359 => Self::SSHLL,
            0x35a => Self::SSHLL2,
            0x35b => Self::SSHLLB,
            0x35c => Self::SSHLLT,
            0x35d => Self::SSHR,
            0x35e => Self::SSRA,
            0x35f => Self::SSUBL,
            0x360 => Self::SSUBL2,
            0x361 => Self::SSUBLB,
            0x362 => Self::SSUBLBT,
            0x363 => Self::SSUBLT,
            0x364 => Self::SSUBLTB,
            0x365 => Self::SSUBW,
            0x366 => Self::SSUBW2,
            0x367 => Self::SSUBWB,
            0x368 => Self::SSUBWT,
            0x369 => Self::ST1,
            0x36a => Self::ST1B,
            0x36b => Self::ST1D,
            0x36c => Self::ST1H,
            0x36d => Self::ST1Q,
            0x36e => Self::ST1W,
            0x36f => Self::ST2,
            0x370 => Self::ST2B,
            0x371 => Self::ST2D,
            0x372 => Self::ST2G,
            0x373 => Self::ST2H,
            0x374 => Self::ST2W,
            0x375 => Self::ST3,
            0x376 => Self::ST3B,
            0x377 => Self::ST3D,
            0x378 => Self::ST3H,
            0x379 => Self::ST3W,
            0x37a => Self::ST4,
            0x37b => Self::ST4B,
            0x37c => Self::ST4D,
            0x37d => Self::ST4H,
            0x37e => Self::ST4W,
            0x37f => Self::ST64B,
            0x380 => Self::ST64BV,
            0x381 => Self::ST64BV0,
            0x382 => Self::STADD,
            0x383 => Self::STADDB,
            0x384 => Self::STADDH,
            0x385 => Self::STADDL,
            0x386 => Self::STADDLB,
            0x387 => Self::STADDLH,
            0x388 => Self::STCLR,
            0x389 => Self::STCLRB,
            0x38a => Self::STCLRH,
            0x38b => Self::STCLRL,
            0x38c => Self::STCLRLB,
            0x38d => Self::STCLRLH,
            0x38e => Self::STEOR,
            0x38f => Self::STEORB,
            0x390 => Self::STEORH,
            0x391 => Self::STEORL,
            0x392 => Self::STEORLB,
            0x393 => Self::STEORLH,
            0x394 => Self::STG,
            0x395 => Self::STGM,
            0x396 => Self::STGP,
            0x397 => Self::STLLR,
            0x398 => Self::STLLRB,
            0x399 => Self::STLLRH,
            0x39a => Self::STLR,
            0x39b => Self::STLRB,
            0x39c => Self::STLRH,
            0x39d => Self::STLUR,
            0x39e => Self::STLURB,
            0x39f => Self::STLURH,
            0x3a0 => Self::STLXP,
            0x3a1 => Self::STLXR,
            0x3a2 => Self::STLXRB,
            0x3a3 => Self::STLXRH,
            0x3a4 => Self::STNP,
            0x3a5 => Self::STNT1B,
            0x3a6 => Self::STNT1D,
            0x3a7 => Self::STNT1H,
            0x3a8 => Self::STNT1W,
            0x3a9 => Self::STP,
            0x3aa => Self::STR,
            0x3ab => Self::STRB,
            0x3ac => Self::STRH,
            0x3ad => Self::STSET,
            0x3ae => Self::STSETB,
            0x3af => Self::STSETH,
            0x3b0 => Self::STSETL,
            0x3b1 => Self::STSETLB,
            0x3b2 => Self::STSETLH,
            0x3b3 => Self::STSMAX,
            0x3b4 => Self::STSMAXB,
            0x3b5 => Self::STSMAXH,
            0x3b6 => Self::STSMAXL,
            0x3b7 => Self::STSMAXLB,
            0x3b8 => Self::STSMAXLH,
            0x3b9 => Self::STSMIN,
            0x3ba => Self::STSMINB,
            0x3bb => Self::STSMINH,
            0x3bc => Self::STSMINL,
            0x3bd => Self::STSMINLB,
            0x3be => Self::STSMINLH,
            0x3bf => Self::STTR,
            0x3c0 => Self::STTRB,
            0x3c1 => Self::STTRH,
            0x3c2 => Self::STUMAX,
            0x3c3 => Self::STUMAXB,
            0x3c4 => Self::STUMAXH,
            0x3c5 => Self::STUMAXL,
            0x3c6 => Self::STUMAXLB,
            0x3c7 => Self::STUMAXLH,
            0x3c8 => Self::STUMIN,
            0x3c9 => Self::STUMINB,
            0x3ca => Self::STUMINH,
            0x3cb => Self::STUMINL,
            0x3cc => Self::STUMINLB,
            0x3cd => Self::STUMINLH,
            0x3ce => Self::STUR,
            0x3cf => Self::STURB,
            0x3d0 => Self::STURH,
            0x3d1 => Self::STXP,
            0x3d2 => Self::STXR,
            0x3d3 => Self::STXRB,
            0x3d4 => Self::STXRH,
            0x3d5 => Self::STZ2G,
            0x3d6 => Self::STZG,
            0x3d7 => Self::STZGM,
            0x3d8 => Self::SUB,
            0x3d9 => Self::SUBG,
            0x3da => Self::SUBHN,
            0x3db => Self::SUBHN2,
            0x3dc => Self::SUBHNB,
            0x3dd => Self::SUBHNT,
            0x3de => Self::SUBP,
            0x3df => Self::SUBPS,
            0x3e0 => Self::SUBR,
            0x3e1 => Self::SUBS,
            0x3e2 => Self::SUDOT,
            0x3e3 => Self::SUMOPA,
            0x3e4 => Self::SUMOPS,
            0x3e5 => Self::SUNPKHI,
            0x3e6 => Self::SUNPKLO,
            0x3e7 => Self::SUQADD,
            0x3e8 => Self::SVC,
            0x3e9 => Self::SWP,
            0x3ea => Self::SWPA,
            0x3eb => Self::SWPAB,
            0x3ec => Self::SWPAH,
            0x3ed => Self::SWPAL,
            0x3ee => Self::SWPALB,
            0x3ef => Self::SWPALH,
            0x3f0 => Self::SWPB,
            0x3f1 => Self::SWPH,
            0x3f2 => Self::SWPL,
            0x3f3 => Self::SWPLB,
            0x3f4 => Self::SWPLH,
            0x3f5 => Self::SXTB,
            0x3f6 => Self::SXTH,
            0x3f7 => Self::SXTL,
            0x3f8 => Self::SXTL2,
            0x3f9 => Self::SXTW,
            0x3fa => Self::SYS,
            0x3fb => Self::SYSL,
            0x3fc => Self::TBL,
            0x3fd => Self::TBNZ,
            0x3fe => Self::TBX,
            0x3ff => Self::TBZ,
            0x400 => Self::TCANCEL,
            0x401 => Self::TCOMMIT,
            0x402 => Self::TLBI,
            0x403 => Self::TRN1,
            0x404 => Self::TRN2,
            0x405 => Self::TSB,
            0x406 => Self::TST,
            0x407 => Self::TSTART,
            0x408 => Self::TTEST,
            0x409 => Self::UABA,
            0x40a => Self::UABAL,
            0x40b => Self::UABAL2,
            0x40c => Self::UABALB,
            0x40d => Self::UABALT,
            0x40e => Self::UABD,
            0x40f => Self::UABDL,
            0x410 => Self::UABDL2,
            0x411 => Self::UABDLB,
            0x412 => Self::UABDLT,
            0x413 => Self::UADALP,
            0x414 => Self::UADDL,
            0x415 => Self::UADDL2,
            0x416 => Self::UADDLB,
            0x417 => Self::UADDLP,
            0x418 => Self::UADDLT,
            0x419 => Self::UADDLV,
            0x41a => Self::UADDV,
            0x41b => Self::UADDW,
            0x41c => Self::UADDW2,
            0x41d => Self::UADDWB,
            0x41e => Self::UADDWT,
            0x41f => Self::UBFIZ,
            0x420 => Self::UBFM,
            0x421 => Self::UBFX,
            0x422 => Self::UCLAMP,
            0x423 => Self::UCVTF,
            0x424 => Self::UDF,
            0x425 => Self::UDIV,
            0x426 => Self::UDIVR,
            0x427 => Self::UDOT,
            0x428 => Self::UHADD,
            0x429 => Self::UHSUB,
            0x42a => Self::UHSUBR,
            0x42b => Self::UMADDL,
            0x42c => Self::UMAX,
            0x42d => Self::UMAXP,
            0x42e => Self::UMAXV,
            0x42f => Self::UMIN,
            0x430 => Self::UMINP,
            0x431 => Self::UMINV,
            0x432 => Self::UMLAL,
            0x433 => Self::UMLAL2,
            0x434 => Self::UMLALB,
            0x435 => Self::UMLALT,
            0x436 => Self::UMLSL,
            0x437 => Self::UMLSL2,
            0x438 => Self::UMLSLB,
            0x439 => Self::UMLSLT,
            0x43a => Self::UMMLA,
            0x43b => Self::UMNEGL,
            0x43c => Self::UMOPA,
            0x43d => Self::UMOPS,
            0x43e => Self::UMOV,
            0x43f => Self::UMSUBL,
            0x440 => Self::UMULH,
            0x441 => Self::UMULL,
            0x442 => Self::UMULL2,
            0x443 => Self::UMULLB,
            0x444 => Self::UMULLT,
            0x445 => Self::UQADD,
            0x446 => Self::UQDECB,
            0x447 => Self::UQDECD,
            0x448 => Self::UQDECH,
            0x449 => Self::UQDECP,
            0x44a => Self::UQDECW,
            0x44b => Self::UQINCB,
            0x44c => Self::UQINCD,
            0x44d => Self::UQINCH,
            0x44e => Self::UQINCP,
            0x44f => Self::UQINCW,
            0x450 => Self::UQRSHL,
            0x451 => Self::UQRSHLR,
            0x452 => Self::UQRSHRN,
            0x453 => Self::UQRSHRN2,
            0x454 => Self::UQRSHRNB,
            0x455 => Self::UQRSHRNT,
            0x456 => Self::UQSHL,
            0x457 => Self::UQSHLR,
            0x458 => Self::UQSHRN,
            0x459 => Self::UQSHRN2,
            0x45a => Self::UQSHRNB,
            0x45b => Self::UQSHRNT,
            0x45c => Self::UQSUB,
            0x45d => Self::UQSUBR,
            0x45e => Self::UQXTN,
            0x45f => Self::UQXTN2,
            0x460 => Self::UQXTNB,
            0x461 => Self::UQXTNT,
            0x462 => Self::URECPE,
            0x463 => Self::URHADD,
            0x464 => Self::URSHL,
            0x465 => Self::URSHLR,
            0x466 => Self::URSHR,
            0x467 => Self::URSQRTE,
            0x468 => Self::URSRA,
            0x469 => Self::USDOT,
            0x46a => Self::USHL,
            0x46b => Self::USHLL,
            0x46c => Self::USHLL2,
            0x46d => Self::USHLLB,
            0x46e => Self::USHLLT,
            0x46f => Self::USHR,
            0x470 => Self::USMMLA,
            0x471 => Self::USMOPA,
            0x472 => Self::USMOPS,
            0x473 => Self::USQADD,
            0x474 => Self::USRA,
            0x475 => Self::USUBL,
            0x476 => Self::USUBL2,
            0x477 => Self::USUBLB,
            0x478 => Self::USUBLT,
            0x479 => Self::USUBW,
            0x47a => Self::USUBW2,
            0x47b => Self::USUBWB,
            0x47c => Self::USUBWT,
            0x47d => Self::UUNPKHI,
            0x47e => Self::UUNPKLO,
            0x47f => Self::UXTB,
            0x480 => Self::UXTH,
            0x481 => Self::UXTL,
            0x482 => Self::UXTL2,
            0x483 => Self::UXTW,
            0x484 => Self::UZP1,
            0x485 => Self::UZP2,
            0x486 => Self::WFE,
            0x487 => Self::WFET,
            0x488 => Self::WFI,
            0x489 => Self::WFIT,
            0x48a => Self::WHILEGE,
            0x48b => Self::WHILEGT,
            0x48c => Self::WHILEHI,
            0x48d => Self::WHILEHS,
            0x48e => Self::WHILELE,
            0x48f => Self::WHILELO,
            0x490 => Self::WHILELS,
            0x491 => Self::WHILELT,
            0x492 => Self::WHILERW,
            0x493 => Self::WHILEWR,
            0x494 => Self::WRFFR,
            0x495 => Self::XAFLAG,
            0x496 => Self::XAR,
            0x497 => Self::XPACD,
            0x498 => Self::XPACI,
            0x499 => Self::XPACLRI,
            0x49a => Self::XTN,
            0x49b => Self::XTN2,
            0x49c => Self::YIELD,
            0x49d => Self::ZERO,
            0x49e => Self::ZIP1,
            0x49f => Self::ZIP2,

            _ => panic!("Invalid arm64 operation value"),
        }
    }
}

impl LowerHex for OperationType {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        write!(f, "{:x}", *self as usize)
    }
}

impl UpperHex for OperationType {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        write!(f, "{:X}", *self as usize)
    }
}

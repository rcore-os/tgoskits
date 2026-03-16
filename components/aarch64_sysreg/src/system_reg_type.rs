/// Provides implementations of formatting traits for system registers.
///
/// The numbering of `SystemReg` follows the order specified in the Instruction Set Specification (ISS),
/// formatted as `<op0><op2><op1><CRn>00000<CRm>0`.
use core::fmt::{Display, Formatter, LowerHex, Result, UpperHex};

/// System register type
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SystemRegType {
    /// System register OSDTRRX_EL1
    OSDTRRX_EL1      = 0x240000,
    /// System register DBGBVR0_EL1
    DBGBVR0_EL1      = 0x280000,
    /// System register DBGBCR0_EL1
    DBGBCR0_EL1      = 0x2a0000,
    /// System register DBGWVR0_EL1
    DBGWVR0_EL1      = 0x2c0000,
    /// System register DBGWCR0_EL1
    DBGWCR0_EL1      = 0x2e0000,
    /// System register DBGBVR1_EL1
    DBGBVR1_EL1      = 0x280002,
    /// System register DBGBCR1_EL1
    DBGBCR1_EL1      = 0x2a0002,
    /// System register DBGWVR1_EL1
    DBGWVR1_EL1      = 0x2c0002,
    /// System register DBGWCR1_EL1
    DBGWCR1_EL1      = 0x2e0002,
    /// System register MDCCINT_EL1
    MDCCINT_EL1      = 0x200004,
    /// System register MDSCR_EL1
    MDSCR_EL1        = 0x240004,
    /// System register DBGBVR2_EL1
    DBGBVR2_EL1      = 0x280004,
    /// System register DBGBCR2_EL1
    DBGBCR2_EL1      = 0x2a0004,
    /// System register DBGWVR2_EL1
    DBGWVR2_EL1      = 0x2c0004,
    /// System register DBGWCR2_EL1
    DBGWCR2_EL1      = 0x2e0004,
    /// System register OSDTRTX_EL1
    OSDTRTX_EL1      = 0x240006,
    /// System register DBGBVR3_EL1
    DBGBVR3_EL1      = 0x280006,
    /// System register DBGBCR3_EL1
    DBGBCR3_EL1      = 0x2a0006,
    /// System register DBGWVR3_EL1
    DBGWVR3_EL1      = 0x2c0006,
    /// System register DBGWCR3_EL1
    DBGWCR3_EL1      = 0x2e0006,
    /// System register DBGBVR4_EL1
    DBGBVR4_EL1      = 0x280008,
    /// System register DBGBCR4_EL1
    DBGBCR4_EL1      = 0x2a0008,
    /// System register DBGWVR4_EL1
    DBGWVR4_EL1      = 0x2c0008,
    /// System register DBGWCR4_EL1
    DBGWCR4_EL1      = 0x2e0008,
    /// System register DBGBVR5_EL1
    DBGBVR5_EL1      = 0x28000a,
    /// System register DBGBCR5_EL1
    DBGBCR5_EL1      = 0x2a000a,
    /// System register DBGWVR5_EL1
    DBGWVR5_EL1      = 0x2c000a,
    /// System register DBGWCR5_EL1
    DBGWCR5_EL1      = 0x2e000a,
    /// System register OSECCR_EL1
    OSECCR_EL1       = 0x24000c,
    /// System register DBGBVR6_EL1
    DBGBVR6_EL1      = 0x28000c,
    /// System register DBGBCR6_EL1
    DBGBCR6_EL1      = 0x2a000c,
    /// System register DBGWVR6_EL1
    DBGWVR6_EL1      = 0x2c000c,
    /// System register DBGWCR6_EL1
    DBGWCR6_EL1      = 0x2e000c,
    /// System register DBGBVR7_EL1
    DBGBVR7_EL1      = 0x28000e,
    /// System register DBGBCR7_EL1
    DBGBCR7_EL1      = 0x2a000e,
    /// System register DBGWVR7_EL1
    DBGWVR7_EL1      = 0x2c000e,
    /// System register DBGWCR7_EL1
    DBGWCR7_EL1      = 0x2e000e,
    /// System register DBGBVR8_EL1
    DBGBVR8_EL1      = 0x280010,
    /// System register DBGBCR8_EL1
    DBGBCR8_EL1      = 0x2a0010,
    /// System register DBGWVR8_EL1
    DBGWVR8_EL1      = 0x2c0010,
    /// System register DBGWCR8_EL1
    DBGWCR8_EL1      = 0x2e0010,
    /// System register DBGBVR9_EL1
    DBGBVR9_EL1      = 0x280012,
    /// System register DBGBCR9_EL1
    DBGBCR9_EL1      = 0x2a0012,
    /// System register DBGWVR9_EL1
    DBGWVR9_EL1      = 0x2c0012,
    /// System register DBGWCR9_EL1
    DBGWCR9_EL1      = 0x2e0012,
    /// System register DBGBVR10_EL1
    DBGBVR10_EL1     = 0x280014,
    /// System register DBGBCR10_EL1
    DBGBCR10_EL1     = 0x2a0014,
    /// System register DBGWVR10_EL1
    DBGWVR10_EL1     = 0x2c0014,
    /// System register DBGWCR10_EL1
    DBGWCR10_EL1     = 0x2e0014,
    /// System register DBGBVR11_EL1
    DBGBVR11_EL1     = 0x280016,
    /// System register DBGBCR11_EL1
    DBGBCR11_EL1     = 0x2a0016,
    /// System register DBGWVR11_EL1
    DBGWVR11_EL1     = 0x2c0016,
    /// System register DBGWCR11_EL1
    DBGWCR11_EL1     = 0x2e0016,
    /// System register DBGBVR12_EL1
    DBGBVR12_EL1     = 0x280018,
    /// System register DBGBCR12_EL1
    DBGBCR12_EL1     = 0x2a0018,
    /// System register DBGWVR12_EL1
    DBGWVR12_EL1     = 0x2c0018,
    /// System register DBGWCR12_EL1
    DBGWCR12_EL1     = 0x2e0018,
    /// System register DBGBVR13_EL1
    DBGBVR13_EL1     = 0x28001a,
    /// System register DBGBCR13_EL1
    DBGBCR13_EL1     = 0x2a001a,
    /// System register DBGWVR13_EL1
    DBGWVR13_EL1     = 0x2c001a,
    /// System register DBGWCR13_EL1
    DBGWCR13_EL1     = 0x2e001a,
    /// System register DBGBVR14_EL1
    DBGBVR14_EL1     = 0x28001c,
    /// System register DBGBCR14_EL1
    DBGBCR14_EL1     = 0x2a001c,
    /// System register DBGWVR14_EL1
    DBGWVR14_EL1     = 0x2c001c,
    /// System register DBGWCR14_EL1
    DBGWCR14_EL1     = 0x2e001c,
    /// System register DBGBVR15_EL1
    DBGBVR15_EL1     = 0x28001e,
    /// System register DBGBCR15_EL1
    DBGBCR15_EL1     = 0x2a001e,
    /// System register DBGWVR15_EL1
    DBGWVR15_EL1     = 0x2c001e,
    /// System register DBGWCR15_EL1
    DBGWCR15_EL1     = 0x2e001e,
    /// System register OSLAR_EL1
    OSLAR_EL1        = 0x280400,
    /// System register OSDLR_EL1
    OSDLR_EL1        = 0x280406,
    /// System register DBGPRCR_EL1
    DBGPRCR_EL1      = 0x280408,
    /// System register DBGCLAIMSET_EL1
    DBGCLAIMSET_EL1  = 0x2c1c10,
    /// System register DBGCLAIMCLR_EL1
    DBGCLAIMCLR_EL1  = 0x2c1c12,
    /// System register TRCTRACEIDR
    TRCTRACEIDR      = 0x224000,
    /// System register TRCVICTLR
    TRCVICTLR        = 0x244000,
    /// System register TRCSEQEVR0
    TRCSEQEVR0       = 0x284000,
    /// System register TRCCNTRLDVR0
    TRCCNTRLDVR0     = 0x2a4000,
    /// System register TRCIMSPEC0
    TRCIMSPEC0       = 0x2e4000,
    /// System register TRCPRGCTLR
    TRCPRGCTLR       = 0x204002,
    /// System register TRCQCTLR
    TRCQCTLR         = 0x224002,
    /// System register TRCVIIECTLR
    TRCVIIECTLR      = 0x244002,
    /// System register TRCSEQEVR1
    TRCSEQEVR1       = 0x284002,
    /// System register TRCCNTRLDVR1
    TRCCNTRLDVR1     = 0x2a4002,
    /// System register TRCIMSPEC1
    TRCIMSPEC1       = 0x2e4002,
    /// System register TRCPROCSELR
    TRCPROCSELR      = 0x204004,
    /// System register TRCVISSCTLR
    TRCVISSCTLR      = 0x244004,
    /// System register TRCSEQEVR2
    TRCSEQEVR2       = 0x284004,
    /// System register TRCCNTRLDVR2
    TRCCNTRLDVR2     = 0x2a4004,
    /// System register TRCIMSPEC2
    TRCIMSPEC2       = 0x2e4004,
    /// System register TRCVIPCSSCTLR
    TRCVIPCSSCTLR    = 0x244006,
    /// System register TRCCNTRLDVR3
    TRCCNTRLDVR3     = 0x2a4006,
    /// System register TRCIMSPEC3
    TRCIMSPEC3       = 0x2e4006,
    /// System register TRCCONFIGR
    TRCCONFIGR       = 0x204008,
    /// System register TRCCNTCTLR0
    TRCCNTCTLR0      = 0x2a4008,
    /// System register TRCIMSPEC4
    TRCIMSPEC4       = 0x2e4008,
    /// System register TRCCNTCTLR1
    TRCCNTCTLR1      = 0x2a400a,
    /// System register TRCIMSPEC5
    TRCIMSPEC5       = 0x2e400a,
    /// System register TRCAUXCTLR
    TRCAUXCTLR       = 0x20400c,
    /// System register TRCSEQRSTEVR
    TRCSEQRSTEVR     = 0x28400c,
    /// System register TRCCNTCTLR2
    TRCCNTCTLR2      = 0x2a400c,
    /// System register TRCIMSPEC6
    TRCIMSPEC6       = 0x2e400c,
    /// System register TRCSEQSTR
    TRCSEQSTR        = 0x28400e,
    /// System register TRCCNTCTLR3
    TRCCNTCTLR3      = 0x2a400e,
    /// System register TRCIMSPEC7
    TRCIMSPEC7       = 0x2e400e,
    /// System register TRCEVENTCTL0R
    TRCEVENTCTL0R    = 0x204010,
    /// System register TRCVDCTLR
    TRCVDCTLR        = 0x244010,
    /// System register TRCEXTINSELR
    TRCEXTINSELR     = 0x284010,
    /// System register TRCCNTVR0
    TRCCNTVR0        = 0x2a4010,
    /// System register TRCEVENTCTL1R
    TRCEVENTCTL1R    = 0x204012,
    /// System register TRCVDSACCTLR
    TRCVDSACCTLR     = 0x244012,
    /// System register TRCEXTINSELR1
    TRCEXTINSELR1    = 0x284012,
    /// System register TRCCNTVR1
    TRCCNTVR1        = 0x2a4012,
    /// System register TRCRSR
    TRCRSR           = 0x204014,
    /// System register TRCVDARCCTLR
    TRCVDARCCTLR     = 0x244014,
    /// System register TRCEXTINSELR2
    TRCEXTINSELR2    = 0x284014,
    /// System register TRCCNTVR2
    TRCCNTVR2        = 0x2a4014,
    /// System register TRCSTALLCTLR
    TRCSTALLCTLR     = 0x204016,
    /// System register TRCEXTINSELR3
    TRCEXTINSELR3    = 0x284016,
    /// System register TRCCNTVR3
    TRCCNTVR3        = 0x2a4016,
    /// System register TRCTSCTLR
    TRCTSCTLR        = 0x204018,
    /// System register TRCSYNCPR
    TRCSYNCPR        = 0x20401a,
    /// System register TRCCCCTLR
    TRCCCCTLR        = 0x20401c,
    /// System register TRCBBCTLR
    TRCBBCTLR        = 0x20401e,
    /// System register TRCRSCTLR16
    TRCRSCTLR16      = 0x224400,
    /// System register TRCSSCCR0
    TRCSSCCR0        = 0x244400,
    /// System register TRCSSPCICR0
    TRCSSPCICR0      = 0x264400,
    /// System register TRCOSLAR
    TRCOSLAR         = 0x284400,
    /// System register TRCRSCTLR17
    TRCRSCTLR17      = 0x224402,
    /// System register TRCSSCCR1
    TRCSSCCR1        = 0x244402,
    /// System register TRCSSPCICR1
    TRCSSPCICR1      = 0x264402,
    /// System register TRCRSCTLR2
    TRCRSCTLR2       = 0x204404,
    /// System register TRCRSCTLR18
    TRCRSCTLR18      = 0x224404,
    /// System register TRCSSCCR2
    TRCSSCCR2        = 0x244404,
    /// System register TRCSSPCICR2
    TRCSSPCICR2      = 0x264404,
    /// System register TRCRSCTLR3
    TRCRSCTLR3       = 0x204406,
    /// System register TRCRSCTLR19
    TRCRSCTLR19      = 0x224406,
    /// System register TRCSSCCR3
    TRCSSCCR3        = 0x244406,
    /// System register TRCSSPCICR3
    TRCSSPCICR3      = 0x264406,
    /// System register TRCRSCTLR4
    TRCRSCTLR4       = 0x204408,
    /// System register TRCRSCTLR20
    TRCRSCTLR20      = 0x224408,
    /// System register TRCSSCCR4
    TRCSSCCR4        = 0x244408,
    /// System register TRCSSPCICR4
    TRCSSPCICR4      = 0x264408,
    /// System register TRCPDCR
    TRCPDCR          = 0x284408,
    /// System register TRCRSCTLR5
    TRCRSCTLR5       = 0x20440a,
    /// System register TRCRSCTLR21
    TRCRSCTLR21      = 0x22440a,
    /// System register TRCSSCCR5
    TRCSSCCR5        = 0x24440a,
    /// System register TRCSSPCICR5
    TRCSSPCICR5      = 0x26440a,
    /// System register TRCRSCTLR6
    TRCRSCTLR6       = 0x20440c,
    /// System register TRCRSCTLR22
    TRCRSCTLR22      = 0x22440c,
    /// System register TRCSSCCR6
    TRCSSCCR6        = 0x24440c,
    /// System register TRCSSPCICR6
    TRCSSPCICR6      = 0x26440c,
    /// System register TRCRSCTLR7
    TRCRSCTLR7       = 0x20440e,
    /// System register TRCRSCTLR23
    TRCRSCTLR23      = 0x22440e,
    /// System register TRCSSCCR7
    TRCSSCCR7        = 0x24440e,
    /// System register TRCSSPCICR7
    TRCSSPCICR7      = 0x26440e,
    /// System register TRCRSCTLR8
    TRCRSCTLR8       = 0x204410,
    /// System register TRCRSCTLR24
    TRCRSCTLR24      = 0x224410,
    /// System register TRCSSCSR0
    TRCSSCSR0        = 0x244410,
    /// System register TRCRSCTLR9
    TRCRSCTLR9       = 0x204412,
    /// System register TRCRSCTLR25
    TRCRSCTLR25      = 0x224412,
    /// System register TRCSSCSR1
    TRCSSCSR1        = 0x244412,
    /// System register TRCRSCTLR10
    TRCRSCTLR10      = 0x204414,
    /// System register TRCRSCTLR26
    TRCRSCTLR26      = 0x224414,
    /// System register TRCSSCSR2
    TRCSSCSR2        = 0x244414,
    /// System register TRCRSCTLR11
    TRCRSCTLR11      = 0x204416,
    /// System register TRCRSCTLR27
    TRCRSCTLR27      = 0x224416,
    /// System register TRCSSCSR3
    TRCSSCSR3        = 0x244416,
    /// System register TRCRSCTLR12
    TRCRSCTLR12      = 0x204418,
    /// System register TRCRSCTLR28
    TRCRSCTLR28      = 0x224418,
    /// System register TRCSSCSR4
    TRCSSCSR4        = 0x244418,
    /// System register TRCRSCTLR13
    TRCRSCTLR13      = 0x20441a,
    /// System register TRCRSCTLR29
    TRCRSCTLR29      = 0x22441a,
    /// System register TRCSSCSR5
    TRCSSCSR5        = 0x24441a,
    /// System register TRCRSCTLR14
    TRCRSCTLR14      = 0x20441c,
    /// System register TRCRSCTLR30
    TRCRSCTLR30      = 0x22441c,
    /// System register TRCSSCSR6
    TRCSSCSR6        = 0x24441c,
    /// System register TRCRSCTLR15
    TRCRSCTLR15      = 0x20441e,
    /// System register TRCRSCTLR31
    TRCRSCTLR31      = 0x22441e,
    /// System register TRCSSCSR7
    TRCSSCSR7        = 0x24441e,
    /// System register TRCACVR0
    TRCACVR0         = 0x204800,
    /// System register TRCACVR8
    TRCACVR8         = 0x224800,
    /// System register TRCACATR0
    TRCACATR0        = 0x244800,
    /// System register TRCACATR8
    TRCACATR8        = 0x264800,
    /// System register TRCDVCVR0
    TRCDVCVR0        = 0x284800,
    /// System register TRCDVCVR4
    TRCDVCVR4        = 0x2a4800,
    /// System register TRCDVCMR0
    TRCDVCMR0        = 0x2c4800,
    /// System register TRCDVCMR4
    TRCDVCMR4        = 0x2e4800,
    /// System register TRCACVR1
    TRCACVR1         = 0x204804,
    /// System register TRCACVR9
    TRCACVR9         = 0x224804,
    /// System register TRCACATR1
    TRCACATR1        = 0x244804,
    /// System register TRCACATR9
    TRCACATR9        = 0x264804,
    /// System register TRCACVR2
    TRCACVR2         = 0x204808,
    /// System register TRCACVR10
    TRCACVR10        = 0x224808,
    /// System register TRCACATR2
    TRCACATR2        = 0x244808,
    /// System register TRCACATR10
    TRCACATR10       = 0x264808,
    /// System register TRCDVCVR1
    TRCDVCVR1        = 0x284808,
    /// System register TRCDVCVR5
    TRCDVCVR5        = 0x2a4808,
    /// System register TRCDVCMR1
    TRCDVCMR1        = 0x2c4808,
    /// System register TRCDVCMR5
    TRCDVCMR5        = 0x2e4808,
    /// System register TRCACVR3
    TRCACVR3         = 0x20480c,
    /// System register TRCACVR11
    TRCACVR11        = 0x22480c,
    /// System register TRCACATR3
    TRCACATR3        = 0x24480c,
    /// System register TRCACATR11
    TRCACATR11       = 0x26480c,
    /// System register TRCACVR4
    TRCACVR4         = 0x204810,
    /// System register TRCACVR12
    TRCACVR12        = 0x224810,
    /// System register TRCACATR4
    TRCACATR4        = 0x244810,
    /// System register TRCACATR12
    TRCACATR12       = 0x264810,
    /// System register TRCDVCVR2
    TRCDVCVR2        = 0x284810,
    /// System register TRCDVCVR6
    TRCDVCVR6        = 0x2a4810,
    /// System register TRCDVCMR2
    TRCDVCMR2        = 0x2c4810,
    /// System register TRCDVCMR6
    TRCDVCMR6        = 0x2e4810,
    /// System register TRCACVR5
    TRCACVR5         = 0x204814,
    /// System register TRCACVR13
    TRCACVR13        = 0x224814,
    /// System register TRCACATR5
    TRCACATR5        = 0x244814,
    /// System register TRCACATR13
    TRCACATR13       = 0x264814,
    /// System register TRCACVR6
    TRCACVR6         = 0x204818,
    /// System register TRCACVR14
    TRCACVR14        = 0x224818,
    /// System register TRCACATR6
    TRCACATR6        = 0x244818,
    /// System register TRCACATR14
    TRCACATR14       = 0x264818,
    /// System register TRCDVCVR3
    TRCDVCVR3        = 0x284818,
    /// System register TRCDVCVR7
    TRCDVCVR7        = 0x2a4818,
    /// System register TRCDVCMR3
    TRCDVCMR3        = 0x2c4818,
    /// System register TRCDVCMR7
    TRCDVCMR7        = 0x2e4818,
    /// System register TRCACVR7
    TRCACVR7         = 0x20481c,
    /// System register TRCACVR15
    TRCACVR15        = 0x22481c,
    /// System register TRCACATR7
    TRCACATR7        = 0x24481c,
    /// System register TRCACATR15
    TRCACATR15       = 0x26481c,
    /// System register TRCCIDCVR0
    TRCCIDCVR0       = 0x204c00,
    /// System register TRCVMIDCVR0
    TRCVMIDCVR0      = 0x224c00,
    /// System register TRCCIDCCTLR0
    TRCCIDCCTLR0     = 0x244c00,
    /// System register TRCCIDCCTLR1
    TRCCIDCCTLR1     = 0x244c02,
    /// System register TRCCIDCVR1
    TRCCIDCVR1       = 0x204c04,
    /// System register TRCVMIDCVR1
    TRCVMIDCVR1      = 0x224c04,
    /// System register TRCVMIDCCTLR0
    TRCVMIDCCTLR0    = 0x244c04,
    /// System register TRCVMIDCCTLR1
    TRCVMIDCCTLR1    = 0x244c06,
    /// System register TRCCIDCVR2
    TRCCIDCVR2       = 0x204c08,
    /// System register TRCVMIDCVR2
    TRCVMIDCVR2      = 0x224c08,
    /// System register TRCCIDCVR3
    TRCCIDCVR3       = 0x204c0c,
    /// System register TRCVMIDCVR3
    TRCVMIDCVR3      = 0x224c0c,
    /// System register TRCCIDCVR4
    TRCCIDCVR4       = 0x204c10,
    /// System register TRCVMIDCVR4
    TRCVMIDCVR4      = 0x224c10,
    /// System register TRCCIDCVR5
    TRCCIDCVR5       = 0x204c14,
    /// System register TRCVMIDCVR5
    TRCVMIDCVR5      = 0x224c14,
    /// System register TRCCIDCVR6
    TRCCIDCVR6       = 0x204c18,
    /// System register TRCVMIDCVR6
    TRCVMIDCVR6      = 0x224c18,
    /// System register TRCCIDCVR7
    TRCCIDCVR7       = 0x204c1c,
    /// System register TRCVMIDCVR7
    TRCVMIDCVR7      = 0x224c1c,
    /// System register TRCITCTRL
    TRCITCTRL        = 0x285c00,
    /// System register TRCCLAIMSET
    TRCCLAIMSET      = 0x2c5c10,
    /// System register TRCCLAIMCLR
    TRCCLAIMCLR      = 0x2c5c12,
    /// System register TRCLAR
    TRCLAR           = 0x2c5c18,
    /// System register TEECR32_EL1
    TEECR32_EL1      = 0x208000,
    /// System register TEEHBR32_EL1
    TEEHBR32_EL1     = 0x208400,
    /// System register DBGDTR_EL0
    DBGDTR_EL0       = 0x20c008,
    /// System register DBGDTRTX_EL0
    DBGDTRTX_EL0     = 0x20c00a,
    /// System register DBGVCR32_EL2
    DBGVCR32_EL2     = 0x21000e,
    /// System register SCTLR_EL1
    SCTLR_EL1        = 0x300400,
    /// System register ACTLR_EL1
    ACTLR_EL1        = 0x320400,
    /// System register CPACR_EL1
    CPACR_EL1        = 0x340400,
    /// System register RGSR_EL1
    RGSR_EL1         = 0x3a0400,
    /// System register GCR_EL1
    GCR_EL1          = 0x3c0400,
    /// System register TRFCR_EL1
    TRFCR_EL1        = 0x320404,
    /// System register TTBR0_EL1
    TTBR0_EL1        = 0x300800,
    /// System register TTBR1_EL1
    TTBR1_EL1        = 0x320800,
    /// System register TCR_EL1
    TCR_EL1          = 0x340800,
    /// System register APIAKEYLO_EL1
    APIAKEYLO_EL1    = 0x300802,
    /// System register APIAKEYHI_EL1
    APIAKEYHI_EL1    = 0x320802,
    /// System register APIBKEYLO_EL1
    APIBKEYLO_EL1    = 0x340802,
    /// System register APIBKEYHI_EL1
    APIBKEYHI_EL1    = 0x360802,
    /// System register APDAKEYLO_EL1
    APDAKEYLO_EL1    = 0x300804,
    /// System register APDAKEYHI_EL1
    APDAKEYHI_EL1    = 0x320804,
    /// System register APDBKEYLO_EL1
    APDBKEYLO_EL1    = 0x340804,
    /// System register APDBKEYHI_EL1
    APDBKEYHI_EL1    = 0x360804,
    /// System register APGAKEYLO_EL1
    APGAKEYLO_EL1    = 0x300806,
    /// System register APGAKEYHI_EL1
    APGAKEYHI_EL1    = 0x320806,
    /// System register SPSR_EL1
    SPSR_EL1         = 0x301000,
    /// System register ELR_EL1
    ELR_EL1          = 0x321000,
    /// System register SP_EL0
    SP_EL0           = 0x301002,
    /// System register SPSEL
    SPSEL            = 0x301004,
    /// System register CURRENTEL
    CURRENTEL        = 0x341004,
    /// System register PAN
    PAN              = 0x361004,
    /// System register UAO
    UAO              = 0x381004,
    /// System register ICC_PMR_EL1
    ICC_PMR_EL1      = 0x30100c,
    /// System register AFSR0_EL1
    AFSR0_EL1        = 0x301402,
    /// System register AFSR1_EL1
    AFSR1_EL1        = 0x321402,
    /// System register ESR_EL1
    ESR_EL1          = 0x301404,
    /// System register ERRSELR_EL1
    ERRSELR_EL1      = 0x321406,
    /// System register ERXCTLR_EL1
    ERXCTLR_EL1      = 0x321408,
    /// System register ERXSTATUS_EL1
    ERXSTATUS_EL1    = 0x341408,
    /// System register ERXADDR_EL1
    ERXADDR_EL1      = 0x361408,
    /// System register ERXPFGCTL_EL1
    ERXPFGCTL_EL1    = 0x3a1408,
    /// System register ERXPFGCDN_EL1
    ERXPFGCDN_EL1    = 0x3c1408,
    /// System register ERXMISC0_EL1
    ERXMISC0_EL1     = 0x30140a,
    /// System register ERXMISC1_EL1
    ERXMISC1_EL1     = 0x32140a,
    /// System register ERXMISC2_EL1
    ERXMISC2_EL1     = 0x34140a,
    /// System register ERXMISC3_EL1
    ERXMISC3_EL1     = 0x36140a,
    /// System register ERXTS_EL1
    ERXTS_EL1        = 0x3e140a,
    /// System register TFSR_EL1
    TFSR_EL1         = 0x30140c,
    /// System register TFSRE0_EL1
    TFSRE0_EL1       = 0x32140c,
    /// System register FAR_EL1
    FAR_EL1          = 0x301800,
    /// System register PAR_EL1
    PAR_EL1          = 0x301c08,
    /// System register PMSCR_EL1
    PMSCR_EL1        = 0x302412,
    /// System register PMSICR_EL1
    PMSICR_EL1       = 0x342412,
    /// System register PMSIRR_EL1
    PMSIRR_EL1       = 0x362412,
    /// System register PMSFCR_EL1
    PMSFCR_EL1       = 0x382412,
    /// System register PMSEVFR_EL1
    PMSEVFR_EL1      = 0x3a2412,
    /// System register PMSLATFR_EL1
    PMSLATFR_EL1     = 0x3c2412,
    /// System register PMSIDR_EL1
    PMSIDR_EL1       = 0x3e2412,
    /// System register PMBLIMITR_EL1
    PMBLIMITR_EL1    = 0x302414,
    /// System register PMBPTR_EL1
    PMBPTR_EL1       = 0x322414,
    /// System register PMBSR_EL1
    PMBSR_EL1        = 0x362414,
    /// System register PMBIDR_EL1
    PMBIDR_EL1       = 0x3e2414,
    /// System register TRBLIMITR_EL1
    TRBLIMITR_EL1    = 0x302416,
    /// System register TRBPTR_EL1
    TRBPTR_EL1       = 0x322416,
    /// System register TRBBASER_EL1
    TRBBASER_EL1     = 0x342416,
    /// System register TRBSR_EL1
    TRBSR_EL1        = 0x362416,
    /// System register TRBMAR_EL1
    TRBMAR_EL1       = 0x382416,
    /// System register TRBTRG_EL1
    TRBTRG_EL1       = 0x3c2416,
    /// System register PMINTENSET_EL1
    PMINTENSET_EL1   = 0x32241c,
    /// System register PMINTENCLR_EL1
    PMINTENCLR_EL1   = 0x34241c,
    /// System register PMMIR_EL1
    PMMIR_EL1        = 0x3c241c,
    /// System register MAIR_EL1
    MAIR_EL1         = 0x302804,
    /// System register AMAIR_EL1
    AMAIR_EL1        = 0x302806,
    /// System register LORSA_EL1
    LORSA_EL1        = 0x302808,
    /// System register LOREA_EL1
    LOREA_EL1        = 0x322808,
    /// System register LORN_EL1
    LORN_EL1         = 0x342808,
    /// System register LORC_EL1
    LORC_EL1         = 0x362808,
    /// System register MPAM1_EL1
    MPAM1_EL1        = 0x30280a,
    /// System register MPAM0_EL1
    MPAM0_EL1        = 0x32280a,
    /// System register VBAR_EL1
    VBAR_EL1         = 0x303000,
    /// System register RMR_EL1
    RMR_EL1          = 0x343000,
    /// System register DISR_EL1
    DISR_EL1         = 0x323002,
    /// System register ICC_EOIR0_EL1
    ICC_EOIR0_EL1    = 0x323010,
    /// System register ICC_BPR0_EL1
    ICC_BPR0_EL1     = 0x363010,
    /// System register ICC_AP0R0_EL1
    ICC_AP0R0_EL1    = 0x383010,
    /// System register ICC_AP0R1_EL1
    ICC_AP0R1_EL1    = 0x3a3010,
    /// System register ICC_AP0R2_EL1
    ICC_AP0R2_EL1    = 0x3c3010,
    /// System register ICC_AP0R3_EL1
    ICC_AP0R3_EL1    = 0x3e3010,
    /// System register ICC_AP1R0_EL1
    ICC_AP1R0_EL1    = 0x303012,
    /// System register ICC_AP1R1_EL1
    ICC_AP1R1_EL1    = 0x323012,
    /// System register ICC_AP1R2_EL1
    ICC_AP1R2_EL1    = 0x343012,
    /// System register ICC_AP1R3_EL1
    ICC_AP1R3_EL1    = 0x363012,
    /// System register ICC_DIR_EL1
    ICC_DIR_EL1      = 0x323016,
    /// System register ICC_SGI1R_EL1
    ICC_SGI1R_EL1    = 0x3a3016,
    /// System register ICC_ASGI1R_EL1
    ICC_ASGI1R_EL1   = 0x3c3016,
    /// System register ICC_SGI0R_EL1
    ICC_SGI0R_EL1    = 0x3e3016,
    /// System register ICC_EOIR1_EL1
    ICC_EOIR1_EL1    = 0x323018,
    /// System register ICC_BPR1_EL1
    ICC_BPR1_EL1     = 0x363018,
    /// System register ICC_CTLR_EL1
    ICC_CTLR_EL1     = 0x383018,
    /// System register ICC_SRE_EL1
    ICC_SRE_EL1      = 0x3a3018,
    /// System register ICC_IGRPEN0_EL1
    ICC_IGRPEN0_EL1  = 0x3c3018,
    /// System register ICC_IGRPEN1_EL1
    ICC_IGRPEN1_EL1  = 0x3e3018,
    /// System register ICC_SEIEN_EL1
    ICC_SEIEN_EL1    = 0x30301a,
    /// System register CONTEXTIDR_EL1
    CONTEXTIDR_EL1   = 0x323400,
    /// System register TPIDR_EL1
    TPIDR_EL1        = 0x383400,
    /// System register SCXTNUM_EL1
    SCXTNUM_EL1      = 0x3e3400,
    /// System register CNTKCTL_EL1
    CNTKCTL_EL1      = 0x303802,
    /// System register CSSELR_EL1
    CSSELR_EL1       = 0x308000,
    /// System register NZCV
    NZCV             = 0x30d004,
    /// System register DAIFSET
    DAIFSET          = 0x32d004,
    /// System register DIT
    DIT              = 0x3ad004,
    /// System register SSBS
    SSBS             = 0x3cd004,
    /// System register TCO
    TCO              = 0x3ed004,
    /// System register FPCR
    FPCR             = 0x30d008,
    /// System register FPSR
    FPSR             = 0x32d008,
    /// System register DSPSR_EL0
    DSPSR_EL0        = 0x30d00a,
    /// System register DLR_EL0
    DLR_EL0          = 0x32d00a,
    /// System register PMCR_EL0
    PMCR_EL0         = 0x30e418,
    /// System register PMCNTENSET_EL0
    PMCNTENSET_EL0   = 0x32e418,
    /// System register PMCNTENCLR_EL0
    PMCNTENCLR_EL0   = 0x34e418,
    /// System register PMOVSCLR_EL0
    PMOVSCLR_EL0     = 0x36e418,
    /// System register PMSWINC_EL0
    PMSWINC_EL0      = 0x38e418,
    /// System register PMSELR_EL0
    PMSELR_EL0       = 0x3ae418,
    /// System register PMCCNTR_EL0
    PMCCNTR_EL0      = 0x30e41a,
    /// System register PMXEVTYPER_EL0
    PMXEVTYPER_EL0   = 0x32e41a,
    /// System register PMXEVCNTR_EL0
    PMXEVCNTR_EL0    = 0x34e41a,
    /// System register DAIFCLR
    DAIFCLR          = 0x3ae41a,
    /// System register PMUSERENR_EL0
    PMUSERENR_EL0    = 0x30e41c,
    /// System register PMOVSSET_EL0
    PMOVSSET_EL0     = 0x36e41c,
    /// System register TPIDR_EL0
    TPIDR_EL0        = 0x34f400,
    /// System register TPIDRRO_EL0
    TPIDRRO_EL0      = 0x36f400,
    /// System register SCXTNUM_EL0
    SCXTNUM_EL0      = 0x3ef400,
    /// System register AMCR_EL0
    AMCR_EL0         = 0x30f404,
    /// System register AMUSERENR_EL0
    AMUSERENR_EL0    = 0x36f404,
    /// System register AMCNTENCLR0_EL0
    AMCNTENCLR0_EL0  = 0x38f404,
    /// System register AMCNTENSET0_EL0
    AMCNTENSET0_EL0  = 0x3af404,
    /// System register AMCNTENCLR1_EL0
    AMCNTENCLR1_EL0  = 0x30f406,
    /// System register AMCNTENSET1_EL0
    AMCNTENSET1_EL0  = 0x32f406,
    /// System register AMEVCNTR00_EL0
    AMEVCNTR00_EL0   = 0x30f408,
    /// System register AMEVCNTR01_EL0
    AMEVCNTR01_EL0   = 0x32f408,
    /// System register AMEVCNTR02_EL0
    AMEVCNTR02_EL0   = 0x34f408,
    /// System register AMEVCNTR03_EL0
    AMEVCNTR03_EL0   = 0x36f408,
    /// System register AMEVCNTR10_EL0
    AMEVCNTR10_EL0   = 0x30f418,
    /// System register AMEVCNTR11_EL0
    AMEVCNTR11_EL0   = 0x32f418,
    /// System register AMEVCNTR12_EL0
    AMEVCNTR12_EL0   = 0x34f418,
    /// System register AMEVCNTR13_EL0
    AMEVCNTR13_EL0   = 0x36f418,
    /// System register AMEVCNTR14_EL0
    AMEVCNTR14_EL0   = 0x38f418,
    /// System register AMEVCNTR15_EL0
    AMEVCNTR15_EL0   = 0x3af418,
    /// System register AMEVCNTR16_EL0
    AMEVCNTR16_EL0   = 0x3cf418,
    /// System register AMEVCNTR17_EL0
    AMEVCNTR17_EL0   = 0x3ef418,
    /// System register AMEVCNTR18_EL0
    AMEVCNTR18_EL0   = 0x30f41a,
    /// System register AMEVCNTR19_EL0
    AMEVCNTR19_EL0   = 0x32f41a,
    /// System register AMEVCNTR110_EL0
    AMEVCNTR110_EL0  = 0x34f41a,
    /// System register AMEVCNTR111_EL0
    AMEVCNTR111_EL0  = 0x36f41a,
    /// System register AMEVCNTR112_EL0
    AMEVCNTR112_EL0  = 0x38f41a,
    /// System register AMEVCNTR113_EL0
    AMEVCNTR113_EL0  = 0x3af41a,
    /// System register AMEVCNTR114_EL0
    AMEVCNTR114_EL0  = 0x3cf41a,
    /// System register AMEVCNTR115_EL0
    AMEVCNTR115_EL0  = 0x3ef41a,
    /// System register AMEVTYPER10_EL0
    AMEVTYPER10_EL0  = 0x30f41c,
    /// System register AMEVTYPER11_EL0
    AMEVTYPER11_EL0  = 0x32f41c,
    /// System register AMEVTYPER12_EL0
    AMEVTYPER12_EL0  = 0x34f41c,
    /// System register AMEVTYPER13_EL0
    AMEVTYPER13_EL0  = 0x36f41c,
    /// System register AMEVTYPER14_EL0
    AMEVTYPER14_EL0  = 0x38f41c,
    /// System register AMEVTYPER15_EL0
    AMEVTYPER15_EL0  = 0x3af41c,
    /// System register AMEVTYPER16_EL0
    AMEVTYPER16_EL0  = 0x3cf41c,
    /// System register AMEVTYPER17_EL0
    AMEVTYPER17_EL0  = 0x3ef41c,
    /// System register AMEVTYPER18_EL0
    AMEVTYPER18_EL0  = 0x30f41e,
    /// System register AMEVTYPER19_EL0
    AMEVTYPER19_EL0  = 0x32f41e,
    /// System register AMEVTYPER110_EL0
    AMEVTYPER110_EL0 = 0x34f41e,
    /// System register AMEVTYPER111_EL0
    AMEVTYPER111_EL0 = 0x36f41e,
    /// System register AMEVTYPER112_EL0
    AMEVTYPER112_EL0 = 0x38f41e,
    /// System register AMEVTYPER113_EL0
    AMEVTYPER113_EL0 = 0x3af41e,
    /// System register AMEVTYPER114_EL0
    AMEVTYPER114_EL0 = 0x3cf41e,
    /// System register AMEVTYPER115_EL0
    AMEVTYPER115_EL0 = 0x3ef41e,
    /// System register CNTFRQ_EL0
    CNTFRQ_EL0       = 0x30f800,
    /// System register CNTPCT_EL0
    CNTPCT_EL0       = 0x32f800,
    /// System register CNTP_TVAL_EL0
    CNTP_TVAL_EL0    = 0x30f804,
    /// System register CNTP_CTL_EL0
    CNTP_CTL_EL0     = 0x32f804,
    /// System register CNTP_CVAL_EL0
    CNTP_CVAL_EL0    = 0x34f804,
    /// System register CNTV_TVAL_EL0
    CNTV_TVAL_EL0    = 0x30f806,
    /// System register CNTV_CTL_EL0
    CNTV_CTL_EL0     = 0x32f806,
    /// System register CNTV_CVAL_EL0
    CNTV_CVAL_EL0    = 0x34f806,
    /// System register PMEVCNTR0_EL0
    PMEVCNTR0_EL0    = 0x30f810,
    /// System register PMEVCNTR1_EL0
    PMEVCNTR1_EL0    = 0x32f810,
    /// System register PMEVCNTR2_EL0
    PMEVCNTR2_EL0    = 0x34f810,
    /// System register PMEVCNTR3_EL0
    PMEVCNTR3_EL0    = 0x36f810,
    /// System register PMEVCNTR4_EL0
    PMEVCNTR4_EL0    = 0x38f810,
    /// System register PMEVCNTR5_EL0
    PMEVCNTR5_EL0    = 0x3af810,
    /// System register PMEVCNTR6_EL0
    PMEVCNTR6_EL0    = 0x3cf810,
    /// System register PMEVCNTR7_EL0
    PMEVCNTR7_EL0    = 0x3ef810,
    /// System register PMEVCNTR8_EL0
    PMEVCNTR8_EL0    = 0x30f812,
    /// System register PMEVCNTR9_EL0
    PMEVCNTR9_EL0    = 0x32f812,
    /// System register PMEVCNTR10_EL0
    PMEVCNTR10_EL0   = 0x34f812,
    /// System register PMEVCNTR11_EL0
    PMEVCNTR11_EL0   = 0x36f812,
    /// System register PMEVCNTR12_EL0
    PMEVCNTR12_EL0   = 0x38f812,
    /// System register PMEVCNTR13_EL0
    PMEVCNTR13_EL0   = 0x3af812,
    /// System register PMEVCNTR14_EL0
    PMEVCNTR14_EL0   = 0x3cf812,
    /// System register PMEVCNTR15_EL0
    PMEVCNTR15_EL0   = 0x3ef812,
    /// System register PMEVCNTR16_EL0
    PMEVCNTR16_EL0   = 0x30f814,
    /// System register PMEVCNTR17_EL0
    PMEVCNTR17_EL0   = 0x32f814,
    /// System register PMEVCNTR18_EL0
    PMEVCNTR18_EL0   = 0x34f814,
    /// System register PMEVCNTR19_EL0
    PMEVCNTR19_EL0   = 0x36f814,
    /// System register PMEVCNTR20_EL0
    PMEVCNTR20_EL0   = 0x38f814,
    /// System register PMEVCNTR21_EL0
    PMEVCNTR21_EL0   = 0x3af814,
    /// System register PMEVCNTR22_EL0
    PMEVCNTR22_EL0   = 0x3cf814,
    /// System register PMEVCNTR23_EL0
    PMEVCNTR23_EL0   = 0x3ef814,
    /// System register PMEVCNTR24_EL0
    PMEVCNTR24_EL0   = 0x30f816,
    /// System register PMEVCNTR25_EL0
    PMEVCNTR25_EL0   = 0x32f816,
    /// System register PMEVCNTR26_EL0
    PMEVCNTR26_EL0   = 0x34f816,
    /// System register PMEVCNTR27_EL0
    PMEVCNTR27_EL0   = 0x36f816,
    /// System register PMEVCNTR28_EL0
    PMEVCNTR28_EL0   = 0x38f816,
    /// System register PMEVCNTR29_EL0
    PMEVCNTR29_EL0   = 0x3af816,
    /// System register PMEVCNTR30_EL0
    PMEVCNTR30_EL0   = 0x3cf816,
    /// System register PMEVTYPER0_EL0
    PMEVTYPER0_EL0   = 0x30f818,
    /// System register PMEVTYPER1_EL0
    PMEVTYPER1_EL0   = 0x32f818,
    /// System register PMEVTYPER2_EL0
    PMEVTYPER2_EL0   = 0x34f818,
    /// System register PMEVTYPER3_EL0
    PMEVTYPER3_EL0   = 0x36f818,
    /// System register PMEVTYPER4_EL0
    PMEVTYPER4_EL0   = 0x38f818,
    /// System register PMEVTYPER5_EL0
    PMEVTYPER5_EL0   = 0x3af818,
    /// System register PMEVTYPER6_EL0
    PMEVTYPER6_EL0   = 0x3cf818,
    /// System register PMEVTYPER7_EL0
    PMEVTYPER7_EL0   = 0x3ef818,
    /// System register PMEVTYPER8_EL0
    PMEVTYPER8_EL0   = 0x30f81a,
    /// System register PMEVTYPER9_EL0
    PMEVTYPER9_EL0   = 0x32f81a,
    /// System register PMEVTYPER10_EL0
    PMEVTYPER10_EL0  = 0x34f81a,
    /// System register PMEVTYPER11_EL0
    PMEVTYPER11_EL0  = 0x36f81a,
    /// System register PMEVTYPER12_EL0
    PMEVTYPER12_EL0  = 0x38f81a,
    /// System register PMEVTYPER13_EL0
    PMEVTYPER13_EL0  = 0x3af81a,
    /// System register PMEVTYPER14_EL0
    PMEVTYPER14_EL0  = 0x3cf81a,
    /// System register PMEVTYPER15_EL0
    PMEVTYPER15_EL0  = 0x3ef81a,
    /// System register PMEVTYPER16_EL0
    PMEVTYPER16_EL0  = 0x30f81c,
    /// System register PMEVTYPER17_EL0
    PMEVTYPER17_EL0  = 0x32f81c,
    /// System register PMEVTYPER18_EL0
    PMEVTYPER18_EL0  = 0x34f81c,
    /// System register PMEVTYPER19_EL0
    PMEVTYPER19_EL0  = 0x36f81c,
    /// System register PMEVTYPER20_EL0
    PMEVTYPER20_EL0  = 0x38f81c,
    /// System register PMEVTYPER21_EL0
    PMEVTYPER21_EL0  = 0x3af81c,
    /// System register PMEVTYPER22_EL0
    PMEVTYPER22_EL0  = 0x3cf81c,
    /// System register PMEVTYPER23_EL0
    PMEVTYPER23_EL0  = 0x3ef81c,
    /// System register PMEVTYPER24_EL0
    PMEVTYPER24_EL0  = 0x30f81e,
    /// System register PMEVTYPER25_EL0
    PMEVTYPER25_EL0  = 0x32f81e,
    /// System register PMEVTYPER26_EL0
    PMEVTYPER26_EL0  = 0x34f81e,
    /// System register PMEVTYPER27_EL0
    PMEVTYPER27_EL0  = 0x36f81e,
    /// System register PMEVTYPER28_EL0
    PMEVTYPER28_EL0  = 0x38f81e,
    /// System register PMEVTYPER29_EL0
    PMEVTYPER29_EL0  = 0x3af81e,
    /// System register PMEVTYPER30_EL0
    PMEVTYPER30_EL0  = 0x3cf81e,
    /// System register PMCCFILTR_EL0
    PMCCFILTR_EL0    = 0x3ef81e,
    /// System register VPIDR_EL2
    VPIDR_EL2        = 0x310000,
    /// System register VMPIDR_EL2
    VMPIDR_EL2       = 0x3b0000,
    /// System register SCTLR_EL2
    SCTLR_EL2        = 0x310400,
    /// System register ACTLR_EL2
    ACTLR_EL2        = 0x330400,
    /// System register HCR_EL2
    HCR_EL2          = 0x310402,
    /// System register MDCR_EL2
    MDCR_EL2         = 0x330402,
    /// System register CPTR_EL2
    CPTR_EL2         = 0x350402,
    /// System register HSTR_EL2
    HSTR_EL2         = 0x370402,
    /// System register HACR_EL2
    HACR_EL2         = 0x3f0402,
    /// System register TRFCR_EL2
    TRFCR_EL2        = 0x330404,
    /// System register SDER32_EL2
    SDER32_EL2       = 0x330406,
    /// System register TTBR0_EL2
    TTBR0_EL2        = 0x310800,
    /// System register TTBR1_EL2
    TTBR1_EL2        = 0x330800,
    /// System register TCR_EL2
    TCR_EL2          = 0x350800,
    /// System register VTTBR_EL2
    VTTBR_EL2        = 0x310802,
    /// System register VTCR_EL2
    VTCR_EL2         = 0x350802,
    /// System register VNCR_EL2
    VNCR_EL2         = 0x310804,
    /// System register VSTTBR_EL2
    VSTTBR_EL2       = 0x31080c,
    /// System register VSTCR_EL2
    VSTCR_EL2        = 0x35080c,
    /// System register DACR32_EL2
    DACR32_EL2       = 0x310c00,
    /// System register SPSR_EL2
    SPSR_EL2         = 0x311000,
    /// System register ELR_EL2
    ELR_EL2          = 0x331000,
    /// System register SP_EL1
    SP_EL1           = 0x311002,
    /// System register SPSR_IRQ
    SPSR_IRQ         = 0x311006,
    /// System register SPSR_ABT
    SPSR_ABT         = 0x331006,
    /// System register SPSR_UND
    SPSR_UND         = 0x351006,
    /// System register SPSR_FIQ
    SPSR_FIQ         = 0x371006,
    /// System register IFSR32_EL2
    IFSR32_EL2       = 0x331400,
    /// System register AFSR0_EL2
    AFSR0_EL2        = 0x311402,
    /// System register AFSR1_EL2
    AFSR1_EL2        = 0x331402,
    /// System register ESR_EL2
    ESR_EL2          = 0x311404,
    /// System register VSESR_EL2
    VSESR_EL2        = 0x371404,
    /// System register FPEXC32_EL2
    FPEXC32_EL2      = 0x311406,
    /// System register TFSR_EL2
    TFSR_EL2         = 0x31140c,
    /// System register FAR_EL2
    FAR_EL2          = 0x311800,
    /// System register HPFAR_EL2
    HPFAR_EL2        = 0x391800,
    /// System register PMSCR_EL2
    PMSCR_EL2        = 0x312412,
    /// System register MAIR_EL2
    MAIR_EL2         = 0x312804,
    /// System register AMAIR_EL2
    AMAIR_EL2        = 0x312806,
    /// System register MPAMHCR_EL2
    MPAMHCR_EL2      = 0x312808,
    /// System register MPAMVPMV_EL2
    MPAMVPMV_EL2     = 0x332808,
    /// System register MPAM2_EL2
    MPAM2_EL2        = 0x31280a,
    /// System register MPAMVPM0_EL2
    MPAMVPM0_EL2     = 0x31280c,
    /// System register MPAMVPM1_EL2
    MPAMVPM1_EL2     = 0x33280c,
    /// System register MPAMVPM2_EL2
    MPAMVPM2_EL2     = 0x35280c,
    /// System register MPAMVPM3_EL2
    MPAMVPM3_EL2     = 0x37280c,
    /// System register MPAMVPM4_EL2
    MPAMVPM4_EL2     = 0x39280c,
    /// System register MPAMVPM5_EL2
    MPAMVPM5_EL2     = 0x3b280c,
    /// System register MPAMVPM6_EL2
    MPAMVPM6_EL2     = 0x3d280c,
    /// System register MPAMVPM7_EL2
    MPAMVPM7_EL2     = 0x3f280c,
    /// System register VBAR_EL2
    VBAR_EL2         = 0x313000,
    /// System register RMR_EL2
    RMR_EL2          = 0x353000,
    /// System register VDISR_EL2
    VDISR_EL2        = 0x333002,
    /// System register ICH_AP0R0_EL2
    ICH_AP0R0_EL2    = 0x313010,
    /// System register ICH_AP0R1_EL2
    ICH_AP0R1_EL2    = 0x333010,
    /// System register ICH_AP0R2_EL2
    ICH_AP0R2_EL2    = 0x353010,
    /// System register ICH_AP0R3_EL2
    ICH_AP0R3_EL2    = 0x373010,
    /// System register ICH_AP1R0_EL2
    ICH_AP1R0_EL2    = 0x313012,
    /// System register ICH_AP1R1_EL2
    ICH_AP1R1_EL2    = 0x333012,
    /// System register ICH_AP1R2_EL2
    ICH_AP1R2_EL2    = 0x353012,
    /// System register ICH_AP1R3_EL2
    ICH_AP1R3_EL2    = 0x373012,
    /// System register ICH_VSEIR_EL2
    ICH_VSEIR_EL2    = 0x393012,
    /// System register ICC_SRE_EL2
    ICC_SRE_EL2      = 0x3b3012,
    /// System register ICH_HCR_EL2
    ICH_HCR_EL2      = 0x313016,
    /// System register ICH_MISR_EL2
    ICH_MISR_EL2     = 0x353016,
    /// System register ICH_VMCR_EL2
    ICH_VMCR_EL2     = 0x3f3016,
    /// System register ICH_LR0_EL2
    ICH_LR0_EL2      = 0x313018,
    /// System register ICH_LR1_EL2
    ICH_LR1_EL2      = 0x333018,
    /// System register ICH_LR2_EL2
    ICH_LR2_EL2      = 0x353018,
    /// System register ICH_LR3_EL2
    ICH_LR3_EL2      = 0x373018,
    /// System register ICH_LR4_EL2
    ICH_LR4_EL2      = 0x393018,
    /// System register ICH_LR5_EL2
    ICH_LR5_EL2      = 0x3b3018,
    /// System register ICH_LR6_EL2
    ICH_LR6_EL2      = 0x3d3018,
    /// System register ICH_LR7_EL2
    ICH_LR7_EL2      = 0x3f3018,
    /// System register ICH_LR8_EL2
    ICH_LR8_EL2      = 0x31301a,
    /// System register ICH_LR9_EL2
    ICH_LR9_EL2      = 0x33301a,
    /// System register ICH_LR10_EL2
    ICH_LR10_EL2     = 0x35301a,
    /// System register ICH_LR11_EL2
    ICH_LR11_EL2     = 0x37301a,
    /// System register ICH_LR12_EL2
    ICH_LR12_EL2     = 0x39301a,
    /// System register ICH_LR13_EL2
    ICH_LR13_EL2     = 0x3b301a,
    /// System register ICH_LR14_EL2
    ICH_LR14_EL2     = 0x3d301a,
    /// System register ICH_LR15_EL2
    ICH_LR15_EL2     = 0x3f301a,
    /// System register CONTEXTIDR_EL2
    CONTEXTIDR_EL2   = 0x333400,
    /// System register TPIDR_EL2
    TPIDR_EL2        = 0x353400,
    /// System register SCXTNUM_EL2
    SCXTNUM_EL2      = 0x3f3400,
    /// System register CNTVOFF_EL2
    CNTVOFF_EL2      = 0x373800,
    /// System register CNTHCTL_EL2
    CNTHCTL_EL2      = 0x313802,
    /// System register CNTHP_TVAL_EL2
    CNTHP_TVAL_EL2   = 0x313804,
    /// System register CNTHP_CTL_EL2
    CNTHP_CTL_EL2    = 0x333804,
    /// System register CNTHP_CVAL_EL2
    CNTHP_CVAL_EL2   = 0x353804,
    /// System register CNTHV_TVAL_EL2
    CNTHV_TVAL_EL2   = 0x313806,
    /// System register CNTHV_CTL_EL2
    CNTHV_CTL_EL2    = 0x333806,
    /// System register CNTHV_CVAL_EL2
    CNTHV_CVAL_EL2   = 0x353806,
    /// System register CNTHVS_TVAL_EL2
    CNTHVS_TVAL_EL2  = 0x313808,
    /// System register CNTHVS_CTL_EL2
    CNTHVS_CTL_EL2   = 0x333808,
    /// System register CNTHVS_CVAL_EL2
    CNTHVS_CVAL_EL2  = 0x353808,
    /// System register CNTHPS_TVAL_EL2
    CNTHPS_TVAL_EL2  = 0x31380a,
    /// System register CNTHPS_CTL_EL2
    CNTHPS_CTL_EL2   = 0x33380a,
    /// System register CNTHPS_CVAL_EL2
    CNTHPS_CVAL_EL2  = 0x35380a,
    /// System register SCTLR_EL12
    SCTLR_EL12       = 0x314400,
    /// System register CPACR_EL12
    CPACR_EL12       = 0x354400,
    /// System register TRFCR_EL12
    TRFCR_EL12       = 0x334404,
    /// System register TTBR0_EL12
    TTBR0_EL12       = 0x314800,
    /// System register TTBR1_EL12
    TTBR1_EL12       = 0x334800,
    /// System register TCR_EL12
    TCR_EL12         = 0x354800,
    /// System register SPSR_EL12
    SPSR_EL12        = 0x315000,
    /// System register ELR_EL12
    ELR_EL12         = 0x335000,
    /// System register AFSR0_EL12
    AFSR0_EL12       = 0x315402,
    /// System register AFSR1_EL12
    AFSR1_EL12       = 0x335402,
    /// System register ESR_EL12
    ESR_EL12         = 0x315404,
    /// System register TFSR_EL12
    TFSR_EL12        = 0x31540c,
    /// System register FAR_EL12
    FAR_EL12         = 0x315800,
    /// System register PMSCR_EL12
    PMSCR_EL12       = 0x316412,
    /// System register MAIR_EL12
    MAIR_EL12        = 0x316804,
    /// System register AMAIR_EL12
    AMAIR_EL12       = 0x316806,
    /// System register MPAM1_EL12
    MPAM1_EL12       = 0x31680a,
    /// System register VBAR_EL12
    VBAR_EL12        = 0x317000,
    /// System register CONTEXTIDR_EL12
    CONTEXTIDR_EL12  = 0x337400,
    /// System register SCXTNUM_EL12
    SCXTNUM_EL12     = 0x3f7400,
    /// System register CNTKCTL_EL12
    CNTKCTL_EL12     = 0x317802,
    /// System register CNTP_TVAL_EL02
    CNTP_TVAL_EL02   = 0x317804,
    /// System register CNTP_CTL_EL02
    CNTP_CTL_EL02    = 0x337804,
    /// System register CNTP_CVAL_EL02
    CNTP_CVAL_EL02   = 0x357804,
    /// System register CNTV_TVAL_EL02
    CNTV_TVAL_EL02   = 0x317806,
    /// System register CNTV_CTL_EL02
    CNTV_CTL_EL02    = 0x337806,
    /// System register CNTV_CVAL_EL02
    CNTV_CVAL_EL02   = 0x357806,
    /// System register SCTLR_EL3
    SCTLR_EL3        = 0x318400,
    /// System register ACTLR_EL3
    ACTLR_EL3        = 0x338400,
    /// System register SCR_EL3
    SCR_EL3          = 0x318402,
    /// System register SDER32_EL3
    SDER32_EL3       = 0x338402,
    /// System register CPTR_EL3
    CPTR_EL3         = 0x358402,
    /// System register MDCR_EL3
    MDCR_EL3         = 0x338406,
    /// System register TTBR0_EL3
    TTBR0_EL3        = 0x318800,
    /// System register TCR_EL3
    TCR_EL3          = 0x358800,
    /// System register SPSR_EL3
    SPSR_EL3         = 0x319000,
    /// System register ELR_EL3
    ELR_EL3          = 0x339000,
    /// System register SP_EL2
    SP_EL2           = 0x319002,
    /// System register AFSR0_EL3
    AFSR0_EL3        = 0x319402,
    /// System register AFSR1_EL3
    AFSR1_EL3        = 0x339402,
    /// System register ESR_EL3
    ESR_EL3          = 0x319404,
    /// System register TFSR_EL3
    TFSR_EL3         = 0x31940c,
    /// System register FAR_EL3
    FAR_EL3          = 0x319800,
    /// System register MAIR_EL3
    MAIR_EL3         = 0x31a804,
    /// System register AMAIR_EL3
    AMAIR_EL3        = 0x31a806,
    /// System register MPAM3_EL3
    MPAM3_EL3        = 0x31a80a,
    /// System register VBAR_EL3
    VBAR_EL3         = 0x31b000,
    /// System register RMR_EL3
    RMR_EL3          = 0x35b000,
    /// System register ICC_CTLR_EL3
    ICC_CTLR_EL3     = 0x39b018,
    /// System register ICC_SRE_EL3
    ICC_SRE_EL3      = 0x3bb018,
    /// System register ICC_IGRPEN1_EL3
    ICC_IGRPEN1_EL3  = 0x3fb018,
    /// System register TPIDR_EL3
    TPIDR_EL3        = 0x35b400,
    /// System register SCXTNUM_EL3
    SCXTNUM_EL3      = 0x3fb400,
    /// System register CNTPS_TVAL_EL1
    CNTPS_TVAL_EL1   = 0x31f804,
    /// System register CNTPS_CTL_EL1
    CNTPS_CTL_EL1    = 0x33f804,
    /// System register CNTPS_CVAL_EL1
    CNTPS_CVAL_EL1   = 0x35f804,
    /// System register PSTATE_SPSEL
    PSTATE_SPSEL     = 0x37f804,
}

impl Display for SystemRegType {
    /// Print system register name
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        match self {
            SystemRegType::OSDTRRX_EL1 => write!(f, "OSDTRRX_EL1"),
            SystemRegType::DBGBVR0_EL1 => write!(f, "DBGBVR0_EL1"),
            SystemRegType::DBGBCR0_EL1 => write!(f, "DBGBCR0_EL1"),
            SystemRegType::DBGWVR0_EL1 => write!(f, "DBGWVR0_EL1"),
            SystemRegType::DBGWCR0_EL1 => write!(f, "DBGWCR0_EL1"),
            SystemRegType::DBGBVR1_EL1 => write!(f, "DBGBVR1_EL1"),
            SystemRegType::DBGBCR1_EL1 => write!(f, "DBGBCR1_EL1"),
            SystemRegType::DBGWVR1_EL1 => write!(f, "DBGWVR1_EL1"),
            SystemRegType::DBGWCR1_EL1 => write!(f, "DBGWCR1_EL1"),
            SystemRegType::MDCCINT_EL1 => write!(f, "MDCCINT_EL1"),
            SystemRegType::MDSCR_EL1 => write!(f, "MDSCR_EL1"),
            SystemRegType::DBGBVR2_EL1 => write!(f, "DBGBVR2_EL1"),
            SystemRegType::DBGBCR2_EL1 => write!(f, "DBGBCR2_EL1"),
            SystemRegType::DBGWVR2_EL1 => write!(f, "DBGWVR2_EL1"),
            SystemRegType::DBGWCR2_EL1 => write!(f, "DBGWCR2_EL1"),
            SystemRegType::OSDTRTX_EL1 => write!(f, "OSDTRTX_EL1"),
            SystemRegType::DBGBVR3_EL1 => write!(f, "DBGBVR3_EL1"),
            SystemRegType::DBGBCR3_EL1 => write!(f, "DBGBCR3_EL1"),
            SystemRegType::DBGWVR3_EL1 => write!(f, "DBGWVR3_EL1"),
            SystemRegType::DBGWCR3_EL1 => write!(f, "DBGWCR3_EL1"),
            SystemRegType::DBGBVR4_EL1 => write!(f, "DBGBVR4_EL1"),
            SystemRegType::DBGBCR4_EL1 => write!(f, "DBGBCR4_EL1"),
            SystemRegType::DBGWVR4_EL1 => write!(f, "DBGWVR4_EL1"),
            SystemRegType::DBGWCR4_EL1 => write!(f, "DBGWCR4_EL1"),
            SystemRegType::DBGBVR5_EL1 => write!(f, "DBGBVR5_EL1"),
            SystemRegType::DBGBCR5_EL1 => write!(f, "DBGBCR5_EL1"),
            SystemRegType::DBGWVR5_EL1 => write!(f, "DBGWVR5_EL1"),
            SystemRegType::DBGWCR5_EL1 => write!(f, "DBGWCR5_EL1"),
            SystemRegType::OSECCR_EL1 => write!(f, "OSECCR_EL1"),
            SystemRegType::DBGBVR6_EL1 => write!(f, "DBGBVR6_EL1"),
            SystemRegType::DBGBCR6_EL1 => write!(f, "DBGBCR6_EL1"),
            SystemRegType::DBGWVR6_EL1 => write!(f, "DBGWVR6_EL1"),
            SystemRegType::DBGWCR6_EL1 => write!(f, "DBGWCR6_EL1"),
            SystemRegType::DBGBVR7_EL1 => write!(f, "DBGBVR7_EL1"),
            SystemRegType::DBGBCR7_EL1 => write!(f, "DBGBCR7_EL1"),
            SystemRegType::DBGWVR7_EL1 => write!(f, "DBGWVR7_EL1"),
            SystemRegType::DBGWCR7_EL1 => write!(f, "DBGWCR7_EL1"),
            SystemRegType::DBGBVR8_EL1 => write!(f, "DBGBVR8_EL1"),
            SystemRegType::DBGBCR8_EL1 => write!(f, "DBGBCR8_EL1"),
            SystemRegType::DBGWVR8_EL1 => write!(f, "DBGWVR8_EL1"),
            SystemRegType::DBGWCR8_EL1 => write!(f, "DBGWCR8_EL1"),
            SystemRegType::DBGBVR9_EL1 => write!(f, "DBGBVR9_EL1"),
            SystemRegType::DBGBCR9_EL1 => write!(f, "DBGBCR9_EL1"),
            SystemRegType::DBGWVR9_EL1 => write!(f, "DBGWVR9_EL1"),
            SystemRegType::DBGWCR9_EL1 => write!(f, "DBGWCR9_EL1"),
            SystemRegType::DBGBVR10_EL1 => write!(f, "DBGBVR10_EL1"),
            SystemRegType::DBGBCR10_EL1 => write!(f, "DBGBCR10_EL1"),
            SystemRegType::DBGWVR10_EL1 => write!(f, "DBGWVR10_EL1"),
            SystemRegType::DBGWCR10_EL1 => write!(f, "DBGWCR10_EL1"),
            SystemRegType::DBGBVR11_EL1 => write!(f, "DBGBVR11_EL1"),
            SystemRegType::DBGBCR11_EL1 => write!(f, "DBGBCR11_EL1"),
            SystemRegType::DBGWVR11_EL1 => write!(f, "DBGWVR11_EL1"),
            SystemRegType::DBGWCR11_EL1 => write!(f, "DBGWCR11_EL1"),
            SystemRegType::DBGBVR12_EL1 => write!(f, "DBGBVR12_EL1"),
            SystemRegType::DBGBCR12_EL1 => write!(f, "DBGBCR12_EL1"),
            SystemRegType::DBGWVR12_EL1 => write!(f, "DBGWVR12_EL1"),
            SystemRegType::DBGWCR12_EL1 => write!(f, "DBGWCR12_EL1"),
            SystemRegType::DBGBVR13_EL1 => write!(f, "DBGBVR13_EL1"),
            SystemRegType::DBGBCR13_EL1 => write!(f, "DBGBCR13_EL1"),
            SystemRegType::DBGWVR13_EL1 => write!(f, "DBGWVR13_EL1"),
            SystemRegType::DBGWCR13_EL1 => write!(f, "DBGWCR13_EL1"),
            SystemRegType::DBGBVR14_EL1 => write!(f, "DBGBVR14_EL1"),
            SystemRegType::DBGBCR14_EL1 => write!(f, "DBGBCR14_EL1"),
            SystemRegType::DBGWVR14_EL1 => write!(f, "DBGWVR14_EL1"),
            SystemRegType::DBGWCR14_EL1 => write!(f, "DBGWCR14_EL1"),
            SystemRegType::DBGBVR15_EL1 => write!(f, "DBGBVR15_EL1"),
            SystemRegType::DBGBCR15_EL1 => write!(f, "DBGBCR15_EL1"),
            SystemRegType::DBGWVR15_EL1 => write!(f, "DBGWVR15_EL1"),
            SystemRegType::DBGWCR15_EL1 => write!(f, "DBGWCR15_EL1"),
            SystemRegType::OSLAR_EL1 => write!(f, "OSLAR_EL1"),
            SystemRegType::OSDLR_EL1 => write!(f, "OSDLR_EL1"),
            SystemRegType::DBGPRCR_EL1 => write!(f, "DBGPRCR_EL1"),
            SystemRegType::DBGCLAIMSET_EL1 => write!(f, "DBGCLAIMSET_EL1"),
            SystemRegType::DBGCLAIMCLR_EL1 => write!(f, "DBGCLAIMCLR_EL1"),
            SystemRegType::TRCTRACEIDR => write!(f, "TRCTRACEIDR"),
            SystemRegType::TRCVICTLR => write!(f, "TRCVICTLR"),
            SystemRegType::TRCSEQEVR0 => write!(f, "TRCSEQEVR0"),
            SystemRegType::TRCCNTRLDVR0 => write!(f, "TRCCNTRLDVR0"),
            SystemRegType::TRCIMSPEC0 => write!(f, "TRCIMSPEC0"),
            SystemRegType::TRCPRGCTLR => write!(f, "TRCPRGCTLR"),
            SystemRegType::TRCQCTLR => write!(f, "TRCQCTLR"),
            SystemRegType::TRCVIIECTLR => write!(f, "TRCVIIECTLR"),
            SystemRegType::TRCSEQEVR1 => write!(f, "TRCSEQEVR1"),
            SystemRegType::TRCCNTRLDVR1 => write!(f, "TRCCNTRLDVR1"),
            SystemRegType::TRCIMSPEC1 => write!(f, "TRCIMSPEC1"),
            SystemRegType::TRCPROCSELR => write!(f, "TRCPROCSELR"),
            SystemRegType::TRCVISSCTLR => write!(f, "TRCVISSCTLR"),
            SystemRegType::TRCSEQEVR2 => write!(f, "TRCSEQEVR2"),
            SystemRegType::TRCCNTRLDVR2 => write!(f, "TRCCNTRLDVR2"),
            SystemRegType::TRCIMSPEC2 => write!(f, "TRCIMSPEC2"),
            SystemRegType::TRCVIPCSSCTLR => write!(f, "TRCVIPCSSCTLR"),
            SystemRegType::TRCCNTRLDVR3 => write!(f, "TRCCNTRLDVR3"),
            SystemRegType::TRCIMSPEC3 => write!(f, "TRCIMSPEC3"),
            SystemRegType::TRCCONFIGR => write!(f, "TRCCONFIGR"),
            SystemRegType::TRCCNTCTLR0 => write!(f, "TRCCNTCTLR0"),
            SystemRegType::TRCIMSPEC4 => write!(f, "TRCIMSPEC4"),
            SystemRegType::TRCCNTCTLR1 => write!(f, "TRCCNTCTLR1"),
            SystemRegType::TRCIMSPEC5 => write!(f, "TRCIMSPEC5"),
            SystemRegType::TRCAUXCTLR => write!(f, "TRCAUXCTLR"),
            SystemRegType::TRCSEQRSTEVR => write!(f, "TRCSEQRSTEVR"),
            SystemRegType::TRCCNTCTLR2 => write!(f, "TRCCNTCTLR2"),
            SystemRegType::TRCIMSPEC6 => write!(f, "TRCIMSPEC6"),
            SystemRegType::TRCSEQSTR => write!(f, "TRCSEQSTR"),
            SystemRegType::TRCCNTCTLR3 => write!(f, "TRCCNTCTLR3"),
            SystemRegType::TRCIMSPEC7 => write!(f, "TRCIMSPEC7"),
            SystemRegType::TRCEVENTCTL0R => write!(f, "TRCEVENTCTL0R"),
            SystemRegType::TRCVDCTLR => write!(f, "TRCVDCTLR"),
            SystemRegType::TRCEXTINSELR => write!(f, "TRCEXTINSELR"),
            SystemRegType::TRCCNTVR0 => write!(f, "TRCCNTVR0"),
            SystemRegType::TRCEVENTCTL1R => write!(f, "TRCEVENTCTL1R"),
            SystemRegType::TRCVDSACCTLR => write!(f, "TRCVDSACCTLR"),
            SystemRegType::TRCEXTINSELR1 => write!(f, "TRCEXTINSELR1"),
            SystemRegType::TRCCNTVR1 => write!(f, "TRCCNTVR1"),
            SystemRegType::TRCRSR => write!(f, "TRCRSR"),
            SystemRegType::TRCVDARCCTLR => write!(f, "TRCVDARCCTLR"),
            SystemRegType::TRCEXTINSELR2 => write!(f, "TRCEXTINSELR2"),
            SystemRegType::TRCCNTVR2 => write!(f, "TRCCNTVR2"),
            SystemRegType::TRCSTALLCTLR => write!(f, "TRCSTALLCTLR"),
            SystemRegType::TRCEXTINSELR3 => write!(f, "TRCEXTINSELR3"),
            SystemRegType::TRCCNTVR3 => write!(f, "TRCCNTVR3"),
            SystemRegType::TRCTSCTLR => write!(f, "TRCTSCTLR"),
            SystemRegType::TRCSYNCPR => write!(f, "TRCSYNCPR"),
            SystemRegType::TRCCCCTLR => write!(f, "TRCCCCTLR"),
            SystemRegType::TRCBBCTLR => write!(f, "TRCBBCTLR"),
            SystemRegType::TRCRSCTLR16 => write!(f, "TRCRSCTLR16"),
            SystemRegType::TRCSSCCR0 => write!(f, "TRCSSCCR0"),
            SystemRegType::TRCSSPCICR0 => write!(f, "TRCSSPCICR0"),
            SystemRegType::TRCOSLAR => write!(f, "TRCOSLAR"),
            SystemRegType::TRCRSCTLR17 => write!(f, "TRCRSCTLR17"),
            SystemRegType::TRCSSCCR1 => write!(f, "TRCSSCCR1"),
            SystemRegType::TRCSSPCICR1 => write!(f, "TRCSSPCICR1"),
            SystemRegType::TRCRSCTLR2 => write!(f, "TRCRSCTLR2"),
            SystemRegType::TRCRSCTLR18 => write!(f, "TRCRSCTLR18"),
            SystemRegType::TRCSSCCR2 => write!(f, "TRCSSCCR2"),
            SystemRegType::TRCSSPCICR2 => write!(f, "TRCSSPCICR2"),
            SystemRegType::TRCRSCTLR3 => write!(f, "TRCRSCTLR3"),
            SystemRegType::TRCRSCTLR19 => write!(f, "TRCRSCTLR19"),
            SystemRegType::TRCSSCCR3 => write!(f, "TRCSSCCR3"),
            SystemRegType::TRCSSPCICR3 => write!(f, "TRCSSPCICR3"),
            SystemRegType::TRCRSCTLR4 => write!(f, "TRCRSCTLR4"),
            SystemRegType::TRCRSCTLR20 => write!(f, "TRCRSCTLR20"),
            SystemRegType::TRCSSCCR4 => write!(f, "TRCSSCCR4"),
            SystemRegType::TRCSSPCICR4 => write!(f, "TRCSSPCICR4"),
            SystemRegType::TRCPDCR => write!(f, "TRCPDCR"),
            SystemRegType::TRCRSCTLR5 => write!(f, "TRCRSCTLR5"),
            SystemRegType::TRCRSCTLR21 => write!(f, "TRCRSCTLR21"),
            SystemRegType::TRCSSCCR5 => write!(f, "TRCSSCCR5"),
            SystemRegType::TRCSSPCICR5 => write!(f, "TRCSSPCICR5"),
            SystemRegType::TRCRSCTLR6 => write!(f, "TRCRSCTLR6"),
            SystemRegType::TRCRSCTLR22 => write!(f, "TRCRSCTLR22"),
            SystemRegType::TRCSSCCR6 => write!(f, "TRCSSCCR6"),
            SystemRegType::TRCSSPCICR6 => write!(f, "TRCSSPCICR6"),
            SystemRegType::TRCRSCTLR7 => write!(f, "TRCRSCTLR7"),
            SystemRegType::TRCRSCTLR23 => write!(f, "TRCRSCTLR23"),
            SystemRegType::TRCSSCCR7 => write!(f, "TRCSSCCR7"),
            SystemRegType::TRCSSPCICR7 => write!(f, "TRCSSPCICR7"),
            SystemRegType::TRCRSCTLR8 => write!(f, "TRCRSCTLR8"),
            SystemRegType::TRCRSCTLR24 => write!(f, "TRCRSCTLR24"),
            SystemRegType::TRCSSCSR0 => write!(f, "TRCSSCSR0"),
            SystemRegType::TRCRSCTLR9 => write!(f, "TRCRSCTLR9"),
            SystemRegType::TRCRSCTLR25 => write!(f, "TRCRSCTLR25"),
            SystemRegType::TRCSSCSR1 => write!(f, "TRCSSCSR1"),
            SystemRegType::TRCRSCTLR10 => write!(f, "TRCRSCTLR10"),
            SystemRegType::TRCRSCTLR26 => write!(f, "TRCRSCTLR26"),
            SystemRegType::TRCSSCSR2 => write!(f, "TRCSSCSR2"),
            SystemRegType::TRCRSCTLR11 => write!(f, "TRCRSCTLR11"),
            SystemRegType::TRCRSCTLR27 => write!(f, "TRCRSCTLR27"),
            SystemRegType::TRCSSCSR3 => write!(f, "TRCSSCSR3"),
            SystemRegType::TRCRSCTLR12 => write!(f, "TRCRSCTLR12"),
            SystemRegType::TRCRSCTLR28 => write!(f, "TRCRSCTLR28"),
            SystemRegType::TRCSSCSR4 => write!(f, "TRCSSCSR4"),
            SystemRegType::TRCRSCTLR13 => write!(f, "TRCRSCTLR13"),
            SystemRegType::TRCRSCTLR29 => write!(f, "TRCRSCTLR29"),
            SystemRegType::TRCSSCSR5 => write!(f, "TRCSSCSR5"),
            SystemRegType::TRCRSCTLR14 => write!(f, "TRCRSCTLR14"),
            SystemRegType::TRCRSCTLR30 => write!(f, "TRCRSCTLR30"),
            SystemRegType::TRCSSCSR6 => write!(f, "TRCSSCSR6"),
            SystemRegType::TRCRSCTLR15 => write!(f, "TRCRSCTLR15"),
            SystemRegType::TRCRSCTLR31 => write!(f, "TRCRSCTLR31"),
            SystemRegType::TRCSSCSR7 => write!(f, "TRCSSCSR7"),
            SystemRegType::TRCACVR0 => write!(f, "TRCACVR0"),
            SystemRegType::TRCACVR8 => write!(f, "TRCACVR8"),
            SystemRegType::TRCACATR0 => write!(f, "TRCACATR0"),
            SystemRegType::TRCACATR8 => write!(f, "TRCACATR8"),
            SystemRegType::TRCDVCVR0 => write!(f, "TRCDVCVR0"),
            SystemRegType::TRCDVCVR4 => write!(f, "TRCDVCVR4"),
            SystemRegType::TRCDVCMR0 => write!(f, "TRCDVCMR0"),
            SystemRegType::TRCDVCMR4 => write!(f, "TRCDVCMR4"),
            SystemRegType::TRCACVR1 => write!(f, "TRCACVR1"),
            SystemRegType::TRCACVR9 => write!(f, "TRCACVR9"),
            SystemRegType::TRCACATR1 => write!(f, "TRCACATR1"),
            SystemRegType::TRCACATR9 => write!(f, "TRCACATR9"),
            SystemRegType::TRCACVR2 => write!(f, "TRCACVR2"),
            SystemRegType::TRCACVR10 => write!(f, "TRCACVR10"),
            SystemRegType::TRCACATR2 => write!(f, "TRCACATR2"),
            SystemRegType::TRCACATR10 => write!(f, "TRCACATR10"),
            SystemRegType::TRCDVCVR1 => write!(f, "TRCDVCVR1"),
            SystemRegType::TRCDVCVR5 => write!(f, "TRCDVCVR5"),
            SystemRegType::TRCDVCMR1 => write!(f, "TRCDVCMR1"),
            SystemRegType::TRCDVCMR5 => write!(f, "TRCDVCMR5"),
            SystemRegType::TRCACVR3 => write!(f, "TRCACVR3"),
            SystemRegType::TRCACVR11 => write!(f, "TRCACVR11"),
            SystemRegType::TRCACATR3 => write!(f, "TRCACATR3"),
            SystemRegType::TRCACATR11 => write!(f, "TRCACATR11"),
            SystemRegType::TRCACVR4 => write!(f, "TRCACVR4"),
            SystemRegType::TRCACVR12 => write!(f, "TRCACVR12"),
            SystemRegType::TRCACATR4 => write!(f, "TRCACATR4"),
            SystemRegType::TRCACATR12 => write!(f, "TRCACATR12"),
            SystemRegType::TRCDVCVR2 => write!(f, "TRCDVCVR2"),
            SystemRegType::TRCDVCVR6 => write!(f, "TRCDVCVR6"),
            SystemRegType::TRCDVCMR2 => write!(f, "TRCDVCMR2"),
            SystemRegType::TRCDVCMR6 => write!(f, "TRCDVCMR6"),
            SystemRegType::TRCACVR5 => write!(f, "TRCACVR5"),
            SystemRegType::TRCACVR13 => write!(f, "TRCACVR13"),
            SystemRegType::TRCACATR5 => write!(f, "TRCACATR5"),
            SystemRegType::TRCACATR13 => write!(f, "TRCACATR13"),
            SystemRegType::TRCACVR6 => write!(f, "TRCACVR6"),
            SystemRegType::TRCACVR14 => write!(f, "TRCACVR14"),
            SystemRegType::TRCACATR6 => write!(f, "TRCACATR6"),
            SystemRegType::TRCACATR14 => write!(f, "TRCACATR14"),
            SystemRegType::TRCDVCVR3 => write!(f, "TRCDVCVR3"),
            SystemRegType::TRCDVCVR7 => write!(f, "TRCDVCVR7"),
            SystemRegType::TRCDVCMR3 => write!(f, "TRCDVCMR3"),
            SystemRegType::TRCDVCMR7 => write!(f, "TRCDVCMR7"),
            SystemRegType::TRCACVR7 => write!(f, "TRCACVR7"),
            SystemRegType::TRCACVR15 => write!(f, "TRCACVR15"),
            SystemRegType::TRCACATR7 => write!(f, "TRCACATR7"),
            SystemRegType::TRCACATR15 => write!(f, "TRCACATR15"),
            SystemRegType::TRCCIDCVR0 => write!(f, "TRCCIDCVR0"),
            SystemRegType::TRCVMIDCVR0 => write!(f, "TRCVMIDCVR0"),
            SystemRegType::TRCCIDCCTLR0 => write!(f, "TRCCIDCCTLR0"),
            SystemRegType::TRCCIDCCTLR1 => write!(f, "TRCCIDCCTLR1"),
            SystemRegType::TRCCIDCVR1 => write!(f, "TRCCIDCVR1"),
            SystemRegType::TRCVMIDCVR1 => write!(f, "TRCVMIDCVR1"),
            SystemRegType::TRCVMIDCCTLR0 => write!(f, "TRCVMIDCCTLR0"),
            SystemRegType::TRCVMIDCCTLR1 => write!(f, "TRCVMIDCCTLR1"),
            SystemRegType::TRCCIDCVR2 => write!(f, "TRCCIDCVR2"),
            SystemRegType::TRCVMIDCVR2 => write!(f, "TRCVMIDCVR2"),
            SystemRegType::TRCCIDCVR3 => write!(f, "TRCCIDCVR3"),
            SystemRegType::TRCVMIDCVR3 => write!(f, "TRCVMIDCVR3"),
            SystemRegType::TRCCIDCVR4 => write!(f, "TRCCIDCVR4"),
            SystemRegType::TRCVMIDCVR4 => write!(f, "TRCVMIDCVR4"),
            SystemRegType::TRCCIDCVR5 => write!(f, "TRCCIDCVR5"),
            SystemRegType::TRCVMIDCVR5 => write!(f, "TRCVMIDCVR5"),
            SystemRegType::TRCCIDCVR6 => write!(f, "TRCCIDCVR6"),
            SystemRegType::TRCVMIDCVR6 => write!(f, "TRCVMIDCVR6"),
            SystemRegType::TRCCIDCVR7 => write!(f, "TRCCIDCVR7"),
            SystemRegType::TRCVMIDCVR7 => write!(f, "TRCVMIDCVR7"),
            SystemRegType::TRCITCTRL => write!(f, "TRCITCTRL"),
            SystemRegType::TRCCLAIMSET => write!(f, "TRCCLAIMSET"),
            SystemRegType::TRCCLAIMCLR => write!(f, "TRCCLAIMCLR"),
            SystemRegType::TRCLAR => write!(f, "TRCLAR"),
            SystemRegType::TEECR32_EL1 => write!(f, "TEECR32_EL1"),
            SystemRegType::TEEHBR32_EL1 => write!(f, "TEEHBR32_EL1"),
            SystemRegType::DBGDTR_EL0 => write!(f, "DBGDTR_EL0"),
            SystemRegType::DBGDTRTX_EL0 => write!(f, "DBGDTRTX_EL0"),
            SystemRegType::DBGVCR32_EL2 => write!(f, "DBGVCR32_EL2"),
            SystemRegType::SCTLR_EL1 => write!(f, "SCTLR_EL1"),
            SystemRegType::ACTLR_EL1 => write!(f, "ACTLR_EL1"),
            SystemRegType::CPACR_EL1 => write!(f, "CPACR_EL1"),
            SystemRegType::RGSR_EL1 => write!(f, "RGSR_EL1"),
            SystemRegType::GCR_EL1 => write!(f, "GCR_EL1"),
            SystemRegType::TRFCR_EL1 => write!(f, "TRFCR_EL1"),
            SystemRegType::TTBR0_EL1 => write!(f, "TTBR0_EL1"),
            SystemRegType::TTBR1_EL1 => write!(f, "TTBR1_EL1"),
            SystemRegType::TCR_EL1 => write!(f, "TCR_EL1"),
            SystemRegType::APIAKEYLO_EL1 => write!(f, "APIAKEYLO_EL1"),
            SystemRegType::APIAKEYHI_EL1 => write!(f, "APIAKEYHI_EL1"),
            SystemRegType::APIBKEYLO_EL1 => write!(f, "APIBKEYLO_EL1"),
            SystemRegType::APIBKEYHI_EL1 => write!(f, "APIBKEYHI_EL1"),
            SystemRegType::APDAKEYLO_EL1 => write!(f, "APDAKEYLO_EL1"),
            SystemRegType::APDAKEYHI_EL1 => write!(f, "APDAKEYHI_EL1"),
            SystemRegType::APDBKEYLO_EL1 => write!(f, "APDBKEYLO_EL1"),
            SystemRegType::APDBKEYHI_EL1 => write!(f, "APDBKEYHI_EL1"),
            SystemRegType::APGAKEYLO_EL1 => write!(f, "APGAKEYLO_EL1"),
            SystemRegType::APGAKEYHI_EL1 => write!(f, "APGAKEYHI_EL1"),
            SystemRegType::SPSR_EL1 => write!(f, "SPSR_EL1"),
            SystemRegType::ELR_EL1 => write!(f, "ELR_EL1"),
            SystemRegType::SP_EL0 => write!(f, "SP_EL0"),
            SystemRegType::SPSEL => write!(f, "SPSEL"),
            SystemRegType::CURRENTEL => write!(f, "CURRENTEL"),
            SystemRegType::PAN => write!(f, "PAN"),
            SystemRegType::UAO => write!(f, "UAO"),
            SystemRegType::ICC_PMR_EL1 => write!(f, "ICC_PMR_EL1"),
            SystemRegType::AFSR0_EL1 => write!(f, "AFSR0_EL1"),
            SystemRegType::AFSR1_EL1 => write!(f, "AFSR1_EL1"),
            SystemRegType::ESR_EL1 => write!(f, "ESR_EL1"),
            SystemRegType::ERRSELR_EL1 => write!(f, "ERRSELR_EL1"),
            SystemRegType::ERXCTLR_EL1 => write!(f, "ERXCTLR_EL1"),
            SystemRegType::ERXSTATUS_EL1 => write!(f, "ERXSTATUS_EL1"),
            SystemRegType::ERXADDR_EL1 => write!(f, "ERXADDR_EL1"),
            SystemRegType::ERXPFGCTL_EL1 => write!(f, "ERXPFGCTL_EL1"),
            SystemRegType::ERXPFGCDN_EL1 => write!(f, "ERXPFGCDN_EL1"),
            SystemRegType::ERXMISC0_EL1 => write!(f, "ERXMISC0_EL1"),
            SystemRegType::ERXMISC1_EL1 => write!(f, "ERXMISC1_EL1"),
            SystemRegType::ERXMISC2_EL1 => write!(f, "ERXMISC2_EL1"),
            SystemRegType::ERXMISC3_EL1 => write!(f, "ERXMISC3_EL1"),
            SystemRegType::ERXTS_EL1 => write!(f, "ERXTS_EL1"),
            SystemRegType::TFSR_EL1 => write!(f, "TFSR_EL1"),
            SystemRegType::TFSRE0_EL1 => write!(f, "TFSRE0_EL1"),
            SystemRegType::FAR_EL1 => write!(f, "FAR_EL1"),
            SystemRegType::PAR_EL1 => write!(f, "PAR_EL1"),
            SystemRegType::PMSCR_EL1 => write!(f, "PMSCR_EL1"),
            SystemRegType::PMSICR_EL1 => write!(f, "PMSICR_EL1"),
            SystemRegType::PMSIRR_EL1 => write!(f, "PMSIRR_EL1"),
            SystemRegType::PMSFCR_EL1 => write!(f, "PMSFCR_EL1"),
            SystemRegType::PMSEVFR_EL1 => write!(f, "PMSEVFR_EL1"),
            SystemRegType::PMSLATFR_EL1 => write!(f, "PMSLATFR_EL1"),
            SystemRegType::PMSIDR_EL1 => write!(f, "PMSIDR_EL1"),
            SystemRegType::PMBLIMITR_EL1 => write!(f, "PMBLIMITR_EL1"),
            SystemRegType::PMBPTR_EL1 => write!(f, "PMBPTR_EL1"),
            SystemRegType::PMBSR_EL1 => write!(f, "PMBSR_EL1"),
            SystemRegType::PMBIDR_EL1 => write!(f, "PMBIDR_EL1"),
            SystemRegType::TRBLIMITR_EL1 => write!(f, "TRBLIMITR_EL1"),
            SystemRegType::TRBPTR_EL1 => write!(f, "TRBPTR_EL1"),
            SystemRegType::TRBBASER_EL1 => write!(f, "TRBBASER_EL1"),
            SystemRegType::TRBSR_EL1 => write!(f, "TRBSR_EL1"),
            SystemRegType::TRBMAR_EL1 => write!(f, "TRBMAR_EL1"),
            SystemRegType::TRBTRG_EL1 => write!(f, "TRBTRG_EL1"),
            SystemRegType::PMINTENSET_EL1 => write!(f, "PMINTENSET_EL1"),
            SystemRegType::PMINTENCLR_EL1 => write!(f, "PMINTENCLR_EL1"),
            SystemRegType::PMMIR_EL1 => write!(f, "PMMIR_EL1"),
            SystemRegType::MAIR_EL1 => write!(f, "MAIR_EL1"),
            SystemRegType::AMAIR_EL1 => write!(f, "AMAIR_EL1"),
            SystemRegType::LORSA_EL1 => write!(f, "LORSA_EL1"),
            SystemRegType::LOREA_EL1 => write!(f, "LOREA_EL1"),
            SystemRegType::LORN_EL1 => write!(f, "LORN_EL1"),
            SystemRegType::LORC_EL1 => write!(f, "LORC_EL1"),
            SystemRegType::MPAM1_EL1 => write!(f, "MPAM1_EL1"),
            SystemRegType::MPAM0_EL1 => write!(f, "MPAM0_EL1"),
            SystemRegType::VBAR_EL1 => write!(f, "VBAR_EL1"),
            SystemRegType::RMR_EL1 => write!(f, "RMR_EL1"),
            SystemRegType::DISR_EL1 => write!(f, "DISR_EL1"),
            SystemRegType::ICC_EOIR0_EL1 => write!(f, "ICC_EOIR0_EL1"),
            SystemRegType::ICC_BPR0_EL1 => write!(f, "ICC_BPR0_EL1"),
            SystemRegType::ICC_AP0R0_EL1 => write!(f, "ICC_AP0R0_EL1"),
            SystemRegType::ICC_AP0R1_EL1 => write!(f, "ICC_AP0R1_EL1"),
            SystemRegType::ICC_AP0R2_EL1 => write!(f, "ICC_AP0R2_EL1"),
            SystemRegType::ICC_AP0R3_EL1 => write!(f, "ICC_AP0R3_EL1"),
            SystemRegType::ICC_AP1R0_EL1 => write!(f, "ICC_AP1R0_EL1"),
            SystemRegType::ICC_AP1R1_EL1 => write!(f, "ICC_AP1R1_EL1"),
            SystemRegType::ICC_AP1R2_EL1 => write!(f, "ICC_AP1R2_EL1"),
            SystemRegType::ICC_AP1R3_EL1 => write!(f, "ICC_AP1R3_EL1"),
            SystemRegType::ICC_DIR_EL1 => write!(f, "ICC_DIR_EL1"),
            SystemRegType::ICC_SGI1R_EL1 => write!(f, "ICC_SGI1R_EL1"),
            SystemRegType::ICC_ASGI1R_EL1 => write!(f, "ICC_ASGI1R_EL1"),
            SystemRegType::ICC_SGI0R_EL1 => write!(f, "ICC_SGI0R_EL1"),
            SystemRegType::ICC_EOIR1_EL1 => write!(f, "ICC_EOIR1_EL1"),
            SystemRegType::ICC_BPR1_EL1 => write!(f, "ICC_BPR1_EL1"),
            SystemRegType::ICC_CTLR_EL1 => write!(f, "ICC_CTLR_EL1"),
            SystemRegType::ICC_SRE_EL1 => write!(f, "ICC_SRE_EL1"),
            SystemRegType::ICC_IGRPEN0_EL1 => write!(f, "ICC_IGRPEN0_EL1"),
            SystemRegType::ICC_IGRPEN1_EL1 => write!(f, "ICC_IGRPEN1_EL1"),
            SystemRegType::ICC_SEIEN_EL1 => write!(f, "ICC_SEIEN_EL1"),
            SystemRegType::CONTEXTIDR_EL1 => write!(f, "CONTEXTIDR_EL1"),
            SystemRegType::TPIDR_EL1 => write!(f, "TPIDR_EL1"),
            SystemRegType::SCXTNUM_EL1 => write!(f, "SCXTNUM_EL1"),
            SystemRegType::CNTKCTL_EL1 => write!(f, "CNTKCTL_EL1"),
            SystemRegType::CSSELR_EL1 => write!(f, "CSSELR_EL1"),
            SystemRegType::NZCV => write!(f, "NZCV"),
            SystemRegType::DAIFSET => write!(f, "DAIFSET"),
            SystemRegType::DIT => write!(f, "DIT"),
            SystemRegType::SSBS => write!(f, "SSBS"),
            SystemRegType::TCO => write!(f, "TCO"),
            SystemRegType::FPCR => write!(f, "FPCR"),
            SystemRegType::FPSR => write!(f, "FPSR"),
            SystemRegType::DSPSR_EL0 => write!(f, "DSPSR_EL0"),
            SystemRegType::DLR_EL0 => write!(f, "DLR_EL0"),
            SystemRegType::PMCR_EL0 => write!(f, "PMCR_EL0"),
            SystemRegType::PMCNTENSET_EL0 => write!(f, "PMCNTENSET_EL0"),
            SystemRegType::PMCNTENCLR_EL0 => write!(f, "PMCNTENCLR_EL0"),
            SystemRegType::PMOVSCLR_EL0 => write!(f, "PMOVSCLR_EL0"),
            SystemRegType::PMSWINC_EL0 => write!(f, "PMSWINC_EL0"),
            SystemRegType::PMSELR_EL0 => write!(f, "PMSELR_EL0"),
            SystemRegType::PMCCNTR_EL0 => write!(f, "PMCCNTR_EL0"),
            SystemRegType::PMXEVTYPER_EL0 => write!(f, "PMXEVTYPER_EL0"),
            SystemRegType::PMXEVCNTR_EL0 => write!(f, "PMXEVCNTR_EL0"),
            SystemRegType::DAIFCLR => write!(f, "DAIFCLR"),
            SystemRegType::PMUSERENR_EL0 => write!(f, "PMUSERENR_EL0"),
            SystemRegType::PMOVSSET_EL0 => write!(f, "PMOVSSET_EL0"),
            SystemRegType::TPIDR_EL0 => write!(f, "TPIDR_EL0"),
            SystemRegType::TPIDRRO_EL0 => write!(f, "TPIDRRO_EL0"),
            SystemRegType::SCXTNUM_EL0 => write!(f, "SCXTNUM_EL0"),
            SystemRegType::AMCR_EL0 => write!(f, "AMCR_EL0"),
            SystemRegType::AMUSERENR_EL0 => write!(f, "AMUSERENR_EL0"),
            SystemRegType::AMCNTENCLR0_EL0 => write!(f, "AMCNTENCLR0_EL0"),
            SystemRegType::AMCNTENSET0_EL0 => write!(f, "AMCNTENSET0_EL0"),
            SystemRegType::AMCNTENCLR1_EL0 => write!(f, "AMCNTENCLR1_EL0"),
            SystemRegType::AMCNTENSET1_EL0 => write!(f, "AMCNTENSET1_EL0"),
            SystemRegType::AMEVCNTR00_EL0 => write!(f, "AMEVCNTR00_EL0"),
            SystemRegType::AMEVCNTR01_EL0 => write!(f, "AMEVCNTR01_EL0"),
            SystemRegType::AMEVCNTR02_EL0 => write!(f, "AMEVCNTR02_EL0"),
            SystemRegType::AMEVCNTR03_EL0 => write!(f, "AMEVCNTR03_EL0"),
            SystemRegType::AMEVCNTR10_EL0 => write!(f, "AMEVCNTR10_EL0"),
            SystemRegType::AMEVCNTR11_EL0 => write!(f, "AMEVCNTR11_EL0"),
            SystemRegType::AMEVCNTR12_EL0 => write!(f, "AMEVCNTR12_EL0"),
            SystemRegType::AMEVCNTR13_EL0 => write!(f, "AMEVCNTR13_EL0"),
            SystemRegType::AMEVCNTR14_EL0 => write!(f, "AMEVCNTR14_EL0"),
            SystemRegType::AMEVCNTR15_EL0 => write!(f, "AMEVCNTR15_EL0"),
            SystemRegType::AMEVCNTR16_EL0 => write!(f, "AMEVCNTR16_EL0"),
            SystemRegType::AMEVCNTR17_EL0 => write!(f, "AMEVCNTR17_EL0"),
            SystemRegType::AMEVCNTR18_EL0 => write!(f, "AMEVCNTR18_EL0"),
            SystemRegType::AMEVCNTR19_EL0 => write!(f, "AMEVCNTR19_EL0"),
            SystemRegType::AMEVCNTR110_EL0 => write!(f, "AMEVCNTR110_EL0"),
            SystemRegType::AMEVCNTR111_EL0 => write!(f, "AMEVCNTR111_EL0"),
            SystemRegType::AMEVCNTR112_EL0 => write!(f, "AMEVCNTR112_EL0"),
            SystemRegType::AMEVCNTR113_EL0 => write!(f, "AMEVCNTR113_EL0"),
            SystemRegType::AMEVCNTR114_EL0 => write!(f, "AMEVCNTR114_EL0"),
            SystemRegType::AMEVCNTR115_EL0 => write!(f, "AMEVCNTR115_EL0"),
            SystemRegType::AMEVTYPER10_EL0 => write!(f, "AMEVTYPER10_EL0"),
            SystemRegType::AMEVTYPER11_EL0 => write!(f, "AMEVTYPER11_EL0"),
            SystemRegType::AMEVTYPER12_EL0 => write!(f, "AMEVTYPER12_EL0"),
            SystemRegType::AMEVTYPER13_EL0 => write!(f, "AMEVTYPER13_EL0"),
            SystemRegType::AMEVTYPER14_EL0 => write!(f, "AMEVTYPER14_EL0"),
            SystemRegType::AMEVTYPER15_EL0 => write!(f, "AMEVTYPER15_EL0"),
            SystemRegType::AMEVTYPER16_EL0 => write!(f, "AMEVTYPER16_EL0"),
            SystemRegType::AMEVTYPER17_EL0 => write!(f, "AMEVTYPER17_EL0"),
            SystemRegType::AMEVTYPER18_EL0 => write!(f, "AMEVTYPER18_EL0"),
            SystemRegType::AMEVTYPER19_EL0 => write!(f, "AMEVTYPER19_EL0"),
            SystemRegType::AMEVTYPER110_EL0 => write!(f, "AMEVTYPER110_EL0"),
            SystemRegType::AMEVTYPER111_EL0 => write!(f, "AMEVTYPER111_EL0"),
            SystemRegType::AMEVTYPER112_EL0 => write!(f, "AMEVTYPER112_EL0"),
            SystemRegType::AMEVTYPER113_EL0 => write!(f, "AMEVTYPER113_EL0"),
            SystemRegType::AMEVTYPER114_EL0 => write!(f, "AMEVTYPER114_EL0"),
            SystemRegType::AMEVTYPER115_EL0 => write!(f, "AMEVTYPER115_EL0"),
            SystemRegType::CNTFRQ_EL0 => write!(f, "CNTFRQ_EL0"),
            SystemRegType::CNTPCT_EL0 => write!(f, "CNTPCT_EL0"),
            SystemRegType::CNTP_TVAL_EL0 => write!(f, "CNTP_TVAL_EL0"),
            SystemRegType::CNTP_CTL_EL0 => write!(f, "CNTP_CTL_EL0"),
            SystemRegType::CNTP_CVAL_EL0 => write!(f, "CNTP_CVAL_EL0"),
            SystemRegType::CNTV_TVAL_EL0 => write!(f, "CNTV_TVAL_EL0"),
            SystemRegType::CNTV_CTL_EL0 => write!(f, "CNTV_CTL_EL0"),
            SystemRegType::CNTV_CVAL_EL0 => write!(f, "CNTV_CVAL_EL0"),
            SystemRegType::PMEVCNTR0_EL0 => write!(f, "PMEVCNTR0_EL0"),
            SystemRegType::PMEVCNTR1_EL0 => write!(f, "PMEVCNTR1_EL0"),
            SystemRegType::PMEVCNTR2_EL0 => write!(f, "PMEVCNTR2_EL0"),
            SystemRegType::PMEVCNTR3_EL0 => write!(f, "PMEVCNTR3_EL0"),
            SystemRegType::PMEVCNTR4_EL0 => write!(f, "PMEVCNTR4_EL0"),
            SystemRegType::PMEVCNTR5_EL0 => write!(f, "PMEVCNTR5_EL0"),
            SystemRegType::PMEVCNTR6_EL0 => write!(f, "PMEVCNTR6_EL0"),
            SystemRegType::PMEVCNTR7_EL0 => write!(f, "PMEVCNTR7_EL0"),
            SystemRegType::PMEVCNTR8_EL0 => write!(f, "PMEVCNTR8_EL0"),
            SystemRegType::PMEVCNTR9_EL0 => write!(f, "PMEVCNTR9_EL0"),
            SystemRegType::PMEVCNTR10_EL0 => write!(f, "PMEVCNTR10_EL0"),
            SystemRegType::PMEVCNTR11_EL0 => write!(f, "PMEVCNTR11_EL0"),
            SystemRegType::PMEVCNTR12_EL0 => write!(f, "PMEVCNTR12_EL0"),
            SystemRegType::PMEVCNTR13_EL0 => write!(f, "PMEVCNTR13_EL0"),
            SystemRegType::PMEVCNTR14_EL0 => write!(f, "PMEVCNTR14_EL0"),
            SystemRegType::PMEVCNTR15_EL0 => write!(f, "PMEVCNTR15_EL0"),
            SystemRegType::PMEVCNTR16_EL0 => write!(f, "PMEVCNTR16_EL0"),
            SystemRegType::PMEVCNTR17_EL0 => write!(f, "PMEVCNTR17_EL0"),
            SystemRegType::PMEVCNTR18_EL0 => write!(f, "PMEVCNTR18_EL0"),
            SystemRegType::PMEVCNTR19_EL0 => write!(f, "PMEVCNTR19_EL0"),
            SystemRegType::PMEVCNTR20_EL0 => write!(f, "PMEVCNTR20_EL0"),
            SystemRegType::PMEVCNTR21_EL0 => write!(f, "PMEVCNTR21_EL0"),
            SystemRegType::PMEVCNTR22_EL0 => write!(f, "PMEVCNTR22_EL0"),
            SystemRegType::PMEVCNTR23_EL0 => write!(f, "PMEVCNTR23_EL0"),
            SystemRegType::PMEVCNTR24_EL0 => write!(f, "PMEVCNTR24_EL0"),
            SystemRegType::PMEVCNTR25_EL0 => write!(f, "PMEVCNTR25_EL0"),
            SystemRegType::PMEVCNTR26_EL0 => write!(f, "PMEVCNTR26_EL0"),
            SystemRegType::PMEVCNTR27_EL0 => write!(f, "PMEVCNTR27_EL0"),
            SystemRegType::PMEVCNTR28_EL0 => write!(f, "PMEVCNTR28_EL0"),
            SystemRegType::PMEVCNTR29_EL0 => write!(f, "PMEVCNTR29_EL0"),
            SystemRegType::PMEVCNTR30_EL0 => write!(f, "PMEVCNTR30_EL0"),
            SystemRegType::PMEVTYPER0_EL0 => write!(f, "PMEVTYPER0_EL0"),
            SystemRegType::PMEVTYPER1_EL0 => write!(f, "PMEVTYPER1_EL0"),
            SystemRegType::PMEVTYPER2_EL0 => write!(f, "PMEVTYPER2_EL0"),
            SystemRegType::PMEVTYPER3_EL0 => write!(f, "PMEVTYPER3_EL0"),
            SystemRegType::PMEVTYPER4_EL0 => write!(f, "PMEVTYPER4_EL0"),
            SystemRegType::PMEVTYPER5_EL0 => write!(f, "PMEVTYPER5_EL0"),
            SystemRegType::PMEVTYPER6_EL0 => write!(f, "PMEVTYPER6_EL0"),
            SystemRegType::PMEVTYPER7_EL0 => write!(f, "PMEVTYPER7_EL0"),
            SystemRegType::PMEVTYPER8_EL0 => write!(f, "PMEVTYPER8_EL0"),
            SystemRegType::PMEVTYPER9_EL0 => write!(f, "PMEVTYPER9_EL0"),
            SystemRegType::PMEVTYPER10_EL0 => write!(f, "PMEVTYPER10_EL0"),
            SystemRegType::PMEVTYPER11_EL0 => write!(f, "PMEVTYPER11_EL0"),
            SystemRegType::PMEVTYPER12_EL0 => write!(f, "PMEVTYPER12_EL0"),
            SystemRegType::PMEVTYPER13_EL0 => write!(f, "PMEVTYPER13_EL0"),
            SystemRegType::PMEVTYPER14_EL0 => write!(f, "PMEVTYPER14_EL0"),
            SystemRegType::PMEVTYPER15_EL0 => write!(f, "PMEVTYPER15_EL0"),
            SystemRegType::PMEVTYPER16_EL0 => write!(f, "PMEVTYPER16_EL0"),
            SystemRegType::PMEVTYPER17_EL0 => write!(f, "PMEVTYPER17_EL0"),
            SystemRegType::PMEVTYPER18_EL0 => write!(f, "PMEVTYPER18_EL0"),
            SystemRegType::PMEVTYPER19_EL0 => write!(f, "PMEVTYPER19_EL0"),
            SystemRegType::PMEVTYPER20_EL0 => write!(f, "PMEVTYPER20_EL0"),
            SystemRegType::PMEVTYPER21_EL0 => write!(f, "PMEVTYPER21_EL0"),
            SystemRegType::PMEVTYPER22_EL0 => write!(f, "PMEVTYPER22_EL0"),
            SystemRegType::PMEVTYPER23_EL0 => write!(f, "PMEVTYPER23_EL0"),
            SystemRegType::PMEVTYPER24_EL0 => write!(f, "PMEVTYPER24_EL0"),
            SystemRegType::PMEVTYPER25_EL0 => write!(f, "PMEVTYPER25_EL0"),
            SystemRegType::PMEVTYPER26_EL0 => write!(f, "PMEVTYPER26_EL0"),
            SystemRegType::PMEVTYPER27_EL0 => write!(f, "PMEVTYPER27_EL0"),
            SystemRegType::PMEVTYPER28_EL0 => write!(f, "PMEVTYPER28_EL0"),
            SystemRegType::PMEVTYPER29_EL0 => write!(f, "PMEVTYPER29_EL0"),
            SystemRegType::PMEVTYPER30_EL0 => write!(f, "PMEVTYPER30_EL0"),
            SystemRegType::PMCCFILTR_EL0 => write!(f, "PMCCFILTR_EL0"),
            SystemRegType::VPIDR_EL2 => write!(f, "VPIDR_EL2"),
            SystemRegType::VMPIDR_EL2 => write!(f, "VMPIDR_EL2"),
            SystemRegType::SCTLR_EL2 => write!(f, "SCTLR_EL2"),
            SystemRegType::ACTLR_EL2 => write!(f, "ACTLR_EL2"),
            SystemRegType::HCR_EL2 => write!(f, "HCR_EL2"),
            SystemRegType::MDCR_EL2 => write!(f, "MDCR_EL2"),
            SystemRegType::CPTR_EL2 => write!(f, "CPTR_EL2"),
            SystemRegType::HSTR_EL2 => write!(f, "HSTR_EL2"),
            SystemRegType::HACR_EL2 => write!(f, "HACR_EL2"),
            SystemRegType::TRFCR_EL2 => write!(f, "TRFCR_EL2"),
            SystemRegType::SDER32_EL2 => write!(f, "SDER32_EL2"),
            SystemRegType::TTBR0_EL2 => write!(f, "TTBR0_EL2"),
            SystemRegType::TTBR1_EL2 => write!(f, "TTBR1_EL2"),
            SystemRegType::TCR_EL2 => write!(f, "TCR_EL2"),
            SystemRegType::VTTBR_EL2 => write!(f, "VTTBR_EL2"),
            SystemRegType::VTCR_EL2 => write!(f, "VTCR_EL2"),
            SystemRegType::VNCR_EL2 => write!(f, "VNCR_EL2"),
            SystemRegType::VSTTBR_EL2 => write!(f, "VSTTBR_EL2"),
            SystemRegType::VSTCR_EL2 => write!(f, "VSTCR_EL2"),
            SystemRegType::DACR32_EL2 => write!(f, "DACR32_EL2"),
            SystemRegType::SPSR_EL2 => write!(f, "SPSR_EL2"),
            SystemRegType::ELR_EL2 => write!(f, "ELR_EL2"),
            SystemRegType::SP_EL1 => write!(f, "SP_EL1"),
            SystemRegType::SPSR_IRQ => write!(f, "SPSR_IRQ"),
            SystemRegType::SPSR_ABT => write!(f, "SPSR_ABT"),
            SystemRegType::SPSR_UND => write!(f, "SPSR_UND"),
            SystemRegType::SPSR_FIQ => write!(f, "SPSR_FIQ"),
            SystemRegType::IFSR32_EL2 => write!(f, "IFSR32_EL2"),
            SystemRegType::AFSR0_EL2 => write!(f, "AFSR0_EL2"),
            SystemRegType::AFSR1_EL2 => write!(f, "AFSR1_EL2"),
            SystemRegType::ESR_EL2 => write!(f, "ESR_EL2"),
            SystemRegType::VSESR_EL2 => write!(f, "VSESR_EL2"),
            SystemRegType::FPEXC32_EL2 => write!(f, "FPEXC32_EL2"),
            SystemRegType::TFSR_EL2 => write!(f, "TFSR_EL2"),
            SystemRegType::FAR_EL2 => write!(f, "FAR_EL2"),
            SystemRegType::HPFAR_EL2 => write!(f, "HPFAR_EL2"),
            SystemRegType::PMSCR_EL2 => write!(f, "PMSCR_EL2"),
            SystemRegType::MAIR_EL2 => write!(f, "MAIR_EL2"),
            SystemRegType::AMAIR_EL2 => write!(f, "AMAIR_EL2"),
            SystemRegType::MPAMHCR_EL2 => write!(f, "MPAMHCR_EL2"),
            SystemRegType::MPAMVPMV_EL2 => write!(f, "MPAMVPMV_EL2"),
            SystemRegType::MPAM2_EL2 => write!(f, "MPAM2_EL2"),
            SystemRegType::MPAMVPM0_EL2 => write!(f, "MPAMVPM0_EL2"),
            SystemRegType::MPAMVPM1_EL2 => write!(f, "MPAMVPM1_EL2"),
            SystemRegType::MPAMVPM2_EL2 => write!(f, "MPAMVPM2_EL2"),
            SystemRegType::MPAMVPM3_EL2 => write!(f, "MPAMVPM3_EL2"),
            SystemRegType::MPAMVPM4_EL2 => write!(f, "MPAMVPM4_EL2"),
            SystemRegType::MPAMVPM5_EL2 => write!(f, "MPAMVPM5_EL2"),
            SystemRegType::MPAMVPM6_EL2 => write!(f, "MPAMVPM6_EL2"),
            SystemRegType::MPAMVPM7_EL2 => write!(f, "MPAMVPM7_EL2"),
            SystemRegType::VBAR_EL2 => write!(f, "VBAR_EL2"),
            SystemRegType::RMR_EL2 => write!(f, "RMR_EL2"),
            SystemRegType::VDISR_EL2 => write!(f, "VDISR_EL2"),
            SystemRegType::ICH_AP0R0_EL2 => write!(f, "ICH_AP0R0_EL2"),
            SystemRegType::ICH_AP0R1_EL2 => write!(f, "ICH_AP0R1_EL2"),
            SystemRegType::ICH_AP0R2_EL2 => write!(f, "ICH_AP0R2_EL2"),
            SystemRegType::ICH_AP0R3_EL2 => write!(f, "ICH_AP0R3_EL2"),
            SystemRegType::ICH_AP1R0_EL2 => write!(f, "ICH_AP1R0_EL2"),
            SystemRegType::ICH_AP1R1_EL2 => write!(f, "ICH_AP1R1_EL2"),
            SystemRegType::ICH_AP1R2_EL2 => write!(f, "ICH_AP1R2_EL2"),
            SystemRegType::ICH_AP1R3_EL2 => write!(f, "ICH_AP1R3_EL2"),
            SystemRegType::ICH_VSEIR_EL2 => write!(f, "ICH_VSEIR_EL2"),
            SystemRegType::ICC_SRE_EL2 => write!(f, "ICC_SRE_EL2"),
            SystemRegType::ICH_HCR_EL2 => write!(f, "ICH_HCR_EL2"),
            SystemRegType::ICH_MISR_EL2 => write!(f, "ICH_MISR_EL2"),
            SystemRegType::ICH_VMCR_EL2 => write!(f, "ICH_VMCR_EL2"),
            SystemRegType::ICH_LR0_EL2 => write!(f, "ICH_LR0_EL2"),
            SystemRegType::ICH_LR1_EL2 => write!(f, "ICH_LR1_EL2"),
            SystemRegType::ICH_LR2_EL2 => write!(f, "ICH_LR2_EL2"),
            SystemRegType::ICH_LR3_EL2 => write!(f, "ICH_LR3_EL2"),
            SystemRegType::ICH_LR4_EL2 => write!(f, "ICH_LR4_EL2"),
            SystemRegType::ICH_LR5_EL2 => write!(f, "ICH_LR5_EL2"),
            SystemRegType::ICH_LR6_EL2 => write!(f, "ICH_LR6_EL2"),
            SystemRegType::ICH_LR7_EL2 => write!(f, "ICH_LR7_EL2"),
            SystemRegType::ICH_LR8_EL2 => write!(f, "ICH_LR8_EL2"),
            SystemRegType::ICH_LR9_EL2 => write!(f, "ICH_LR9_EL2"),
            SystemRegType::ICH_LR10_EL2 => write!(f, "ICH_LR10_EL2"),
            SystemRegType::ICH_LR11_EL2 => write!(f, "ICH_LR11_EL2"),
            SystemRegType::ICH_LR12_EL2 => write!(f, "ICH_LR12_EL2"),
            SystemRegType::ICH_LR13_EL2 => write!(f, "ICH_LR13_EL2"),
            SystemRegType::ICH_LR14_EL2 => write!(f, "ICH_LR14_EL2"),
            SystemRegType::ICH_LR15_EL2 => write!(f, "ICH_LR15_EL2"),
            SystemRegType::CONTEXTIDR_EL2 => write!(f, "CONTEXTIDR_EL2"),
            SystemRegType::TPIDR_EL2 => write!(f, "TPIDR_EL2"),
            SystemRegType::SCXTNUM_EL2 => write!(f, "SCXTNUM_EL2"),
            SystemRegType::CNTVOFF_EL2 => write!(f, "CNTVOFF_EL2"),
            SystemRegType::CNTHCTL_EL2 => write!(f, "CNTHCTL_EL2"),
            SystemRegType::CNTHP_TVAL_EL2 => write!(f, "CNTHP_TVAL_EL2"),
            SystemRegType::CNTHP_CTL_EL2 => write!(f, "CNTHP_CTL_EL2"),
            SystemRegType::CNTHP_CVAL_EL2 => write!(f, "CNTHP_CVAL_EL2"),
            SystemRegType::CNTHV_TVAL_EL2 => write!(f, "CNTHV_TVAL_EL2"),
            SystemRegType::CNTHV_CTL_EL2 => write!(f, "CNTHV_CTL_EL2"),
            SystemRegType::CNTHV_CVAL_EL2 => write!(f, "CNTHV_CVAL_EL2"),
            SystemRegType::CNTHVS_TVAL_EL2 => write!(f, "CNTHVS_TVAL_EL2"),
            SystemRegType::CNTHVS_CTL_EL2 => write!(f, "CNTHVS_CTL_EL2"),
            SystemRegType::CNTHVS_CVAL_EL2 => write!(f, "CNTHVS_CVAL_EL2"),
            SystemRegType::CNTHPS_TVAL_EL2 => write!(f, "CNTHPS_TVAL_EL2"),
            SystemRegType::CNTHPS_CTL_EL2 => write!(f, "CNTHPS_CTL_EL2"),
            SystemRegType::CNTHPS_CVAL_EL2 => write!(f, "CNTHPS_CVAL_EL2"),
            SystemRegType::SCTLR_EL12 => write!(f, "SCTLR_EL12"),
            SystemRegType::CPACR_EL12 => write!(f, "CPACR_EL12"),
            SystemRegType::TRFCR_EL12 => write!(f, "TRFCR_EL12"),
            SystemRegType::TTBR0_EL12 => write!(f, "TTBR0_EL12"),
            SystemRegType::TTBR1_EL12 => write!(f, "TTBR1_EL12"),
            SystemRegType::TCR_EL12 => write!(f, "TCR_EL12"),
            SystemRegType::SPSR_EL12 => write!(f, "SPSR_EL12"),
            SystemRegType::ELR_EL12 => write!(f, "ELR_EL12"),
            SystemRegType::AFSR0_EL12 => write!(f, "AFSR0_EL12"),
            SystemRegType::AFSR1_EL12 => write!(f, "AFSR1_EL12"),
            SystemRegType::ESR_EL12 => write!(f, "ESR_EL12"),
            SystemRegType::TFSR_EL12 => write!(f, "TFSR_EL12"),
            SystemRegType::FAR_EL12 => write!(f, "FAR_EL12"),
            SystemRegType::PMSCR_EL12 => write!(f, "PMSCR_EL12"),
            SystemRegType::MAIR_EL12 => write!(f, "MAIR_EL12"),
            SystemRegType::AMAIR_EL12 => write!(f, "AMAIR_EL12"),
            SystemRegType::MPAM1_EL12 => write!(f, "MPAM1_EL12"),
            SystemRegType::VBAR_EL12 => write!(f, "VBAR_EL12"),
            SystemRegType::CONTEXTIDR_EL12 => write!(f, "CONTEXTIDR_EL12"),
            SystemRegType::SCXTNUM_EL12 => write!(f, "SCXTNUM_EL12"),
            SystemRegType::CNTKCTL_EL12 => write!(f, "CNTKCTL_EL12"),
            SystemRegType::CNTP_TVAL_EL02 => write!(f, "CNTP_TVAL_EL02"),
            SystemRegType::CNTP_CTL_EL02 => write!(f, "CNTP_CTL_EL02"),
            SystemRegType::CNTP_CVAL_EL02 => write!(f, "CNTP_CVAL_EL02"),
            SystemRegType::CNTV_TVAL_EL02 => write!(f, "CNTV_TVAL_EL02"),
            SystemRegType::CNTV_CTL_EL02 => write!(f, "CNTV_CTL_EL02"),
            SystemRegType::CNTV_CVAL_EL02 => write!(f, "CNTV_CVAL_EL02"),
            SystemRegType::SCTLR_EL3 => write!(f, "SCTLR_EL3"),
            SystemRegType::ACTLR_EL3 => write!(f, "ACTLR_EL3"),
            SystemRegType::SCR_EL3 => write!(f, "SCR_EL3"),
            SystemRegType::SDER32_EL3 => write!(f, "SDER32_EL3"),
            SystemRegType::CPTR_EL3 => write!(f, "CPTR_EL3"),
            SystemRegType::MDCR_EL3 => write!(f, "MDCR_EL3"),
            SystemRegType::TTBR0_EL3 => write!(f, "TTBR0_EL3"),
            SystemRegType::TCR_EL3 => write!(f, "TCR_EL3"),
            SystemRegType::SPSR_EL3 => write!(f, "SPSR_EL3"),
            SystemRegType::ELR_EL3 => write!(f, "ELR_EL3"),
            SystemRegType::SP_EL2 => write!(f, "SP_EL2"),
            SystemRegType::AFSR0_EL3 => write!(f, "AFSR0_EL3"),
            SystemRegType::AFSR1_EL3 => write!(f, "AFSR1_EL3"),
            SystemRegType::ESR_EL3 => write!(f, "ESR_EL3"),
            SystemRegType::TFSR_EL3 => write!(f, "TFSR_EL3"),
            SystemRegType::FAR_EL3 => write!(f, "FAR_EL3"),
            SystemRegType::MAIR_EL3 => write!(f, "MAIR_EL3"),
            SystemRegType::AMAIR_EL3 => write!(f, "AMAIR_EL3"),
            SystemRegType::MPAM3_EL3 => write!(f, "MPAM3_EL3"),
            SystemRegType::VBAR_EL3 => write!(f, "VBAR_EL3"),
            SystemRegType::RMR_EL3 => write!(f, "RMR_EL3"),
            SystemRegType::ICC_CTLR_EL3 => write!(f, "ICC_CTLR_EL3"),
            SystemRegType::ICC_SRE_EL3 => write!(f, "ICC_SRE_EL3"),
            SystemRegType::ICC_IGRPEN1_EL3 => write!(f, "ICC_IGRPEN1_EL3"),
            SystemRegType::TPIDR_EL3 => write!(f, "TPIDR_EL3"),
            SystemRegType::SCXTNUM_EL3 => write!(f, "SCXTNUM_EL3"),
            SystemRegType::CNTPS_TVAL_EL1 => write!(f, "CNTPS_TVAL_EL1"),
            SystemRegType::CNTPS_CTL_EL1 => write!(f, "CNTPS_CTL_EL1"),
            SystemRegType::CNTPS_CVAL_EL1 => write!(f, "CNTPS_CVAL_EL1"),
            SystemRegType::PSTATE_SPSEL => write!(f, "PSTATE_SPSEL"),
        }
    }
}

impl From<usize> for SystemRegType {
    fn from(value: usize) -> Self {
        match value {
            0x240000 => Self::OSDTRRX_EL1,
            0x280000 => Self::DBGBVR0_EL1,
            0x2a0000 => Self::DBGBCR0_EL1,
            0x2c0000 => Self::DBGWVR0_EL1,
            0x2e0000 => Self::DBGWCR0_EL1,
            0x280002 => Self::DBGBVR1_EL1,
            0x2a0002 => Self::DBGBCR1_EL1,
            0x2c0002 => Self::DBGWVR1_EL1,
            0x2e0002 => Self::DBGWCR1_EL1,
            0x200004 => Self::MDCCINT_EL1,
            0x240004 => Self::MDSCR_EL1,
            0x280004 => Self::DBGBVR2_EL1,
            0x2a0004 => Self::DBGBCR2_EL1,
            0x2c0004 => Self::DBGWVR2_EL1,
            0x2e0004 => Self::DBGWCR2_EL1,
            0x240006 => Self::OSDTRTX_EL1,
            0x280006 => Self::DBGBVR3_EL1,
            0x2a0006 => Self::DBGBCR3_EL1,
            0x2c0006 => Self::DBGWVR3_EL1,
            0x2e0006 => Self::DBGWCR3_EL1,
            0x280008 => Self::DBGBVR4_EL1,
            0x2a0008 => Self::DBGBCR4_EL1,
            0x2c0008 => Self::DBGWVR4_EL1,
            0x2e0008 => Self::DBGWCR4_EL1,
            0x28000a => Self::DBGBVR5_EL1,
            0x2a000a => Self::DBGBCR5_EL1,
            0x2c000a => Self::DBGWVR5_EL1,
            0x2e000a => Self::DBGWCR5_EL1,
            0x24000c => Self::OSECCR_EL1,
            0x28000c => Self::DBGBVR6_EL1,
            0x2a000c => Self::DBGBCR6_EL1,
            0x2c000c => Self::DBGWVR6_EL1,
            0x2e000c => Self::DBGWCR6_EL1,
            0x28000e => Self::DBGBVR7_EL1,
            0x2a000e => Self::DBGBCR7_EL1,
            0x2c000e => Self::DBGWVR7_EL1,
            0x2e000e => Self::DBGWCR7_EL1,
            0x280010 => Self::DBGBVR8_EL1,
            0x2a0010 => Self::DBGBCR8_EL1,
            0x2c0010 => Self::DBGWVR8_EL1,
            0x2e0010 => Self::DBGWCR8_EL1,
            0x280012 => Self::DBGBVR9_EL1,
            0x2a0012 => Self::DBGBCR9_EL1,
            0x2c0012 => Self::DBGWVR9_EL1,
            0x2e0012 => Self::DBGWCR9_EL1,
            0x280014 => Self::DBGBVR10_EL1,
            0x2a0014 => Self::DBGBCR10_EL1,
            0x2c0014 => Self::DBGWVR10_EL1,
            0x2e0014 => Self::DBGWCR10_EL1,
            0x280016 => Self::DBGBVR11_EL1,
            0x2a0016 => Self::DBGBCR11_EL1,
            0x2c0016 => Self::DBGWVR11_EL1,
            0x2e0016 => Self::DBGWCR11_EL1,
            0x280018 => Self::DBGBVR12_EL1,
            0x2a0018 => Self::DBGBCR12_EL1,
            0x2c0018 => Self::DBGWVR12_EL1,
            0x2e0018 => Self::DBGWCR12_EL1,
            0x28001a => Self::DBGBVR13_EL1,
            0x2a001a => Self::DBGBCR13_EL1,
            0x2c001a => Self::DBGWVR13_EL1,
            0x2e001a => Self::DBGWCR13_EL1,
            0x28001c => Self::DBGBVR14_EL1,
            0x2a001c => Self::DBGBCR14_EL1,
            0x2c001c => Self::DBGWVR14_EL1,
            0x2e001c => Self::DBGWCR14_EL1,
            0x28001e => Self::DBGBVR15_EL1,
            0x2a001e => Self::DBGBCR15_EL1,
            0x2c001e => Self::DBGWVR15_EL1,
            0x2e001e => Self::DBGWCR15_EL1,
            0x280400 => Self::OSLAR_EL1,
            0x280406 => Self::OSDLR_EL1,
            0x280408 => Self::DBGPRCR_EL1,
            0x2c1c10 => Self::DBGCLAIMSET_EL1,
            0x2c1c12 => Self::DBGCLAIMCLR_EL1,
            0x224000 => Self::TRCTRACEIDR,
            0x244000 => Self::TRCVICTLR,
            0x284000 => Self::TRCSEQEVR0,
            0x2a4000 => Self::TRCCNTRLDVR0,
            0x2e4000 => Self::TRCIMSPEC0,
            0x204002 => Self::TRCPRGCTLR,
            0x224002 => Self::TRCQCTLR,
            0x244002 => Self::TRCVIIECTLR,
            0x284002 => Self::TRCSEQEVR1,
            0x2a4002 => Self::TRCCNTRLDVR1,
            0x2e4002 => Self::TRCIMSPEC1,
            0x204004 => Self::TRCPROCSELR,
            0x244004 => Self::TRCVISSCTLR,
            0x284004 => Self::TRCSEQEVR2,
            0x2a4004 => Self::TRCCNTRLDVR2,
            0x2e4004 => Self::TRCIMSPEC2,
            0x244006 => Self::TRCVIPCSSCTLR,
            0x2a4006 => Self::TRCCNTRLDVR3,
            0x2e4006 => Self::TRCIMSPEC3,
            0x204008 => Self::TRCCONFIGR,
            0x2a4008 => Self::TRCCNTCTLR0,
            0x2e4008 => Self::TRCIMSPEC4,
            0x2a400a => Self::TRCCNTCTLR1,
            0x2e400a => Self::TRCIMSPEC5,
            0x20400c => Self::TRCAUXCTLR,
            0x28400c => Self::TRCSEQRSTEVR,
            0x2a400c => Self::TRCCNTCTLR2,
            0x2e400c => Self::TRCIMSPEC6,
            0x28400e => Self::TRCSEQSTR,
            0x2a400e => Self::TRCCNTCTLR3,
            0x2e400e => Self::TRCIMSPEC7,
            0x204010 => Self::TRCEVENTCTL0R,
            0x244010 => Self::TRCVDCTLR,
            0x284010 => Self::TRCEXTINSELR,
            0x2a4010 => Self::TRCCNTVR0,
            0x204012 => Self::TRCEVENTCTL1R,
            0x244012 => Self::TRCVDSACCTLR,
            0x284012 => Self::TRCEXTINSELR1,
            0x2a4012 => Self::TRCCNTVR1,
            0x204014 => Self::TRCRSR,
            0x244014 => Self::TRCVDARCCTLR,
            0x284014 => Self::TRCEXTINSELR2,
            0x2a4014 => Self::TRCCNTVR2,
            0x204016 => Self::TRCSTALLCTLR,
            0x284016 => Self::TRCEXTINSELR3,
            0x2a4016 => Self::TRCCNTVR3,
            0x204018 => Self::TRCTSCTLR,
            0x20401a => Self::TRCSYNCPR,
            0x20401c => Self::TRCCCCTLR,
            0x20401e => Self::TRCBBCTLR,
            0x224400 => Self::TRCRSCTLR16,
            0x244400 => Self::TRCSSCCR0,
            0x264400 => Self::TRCSSPCICR0,
            0x284400 => Self::TRCOSLAR,
            0x224402 => Self::TRCRSCTLR17,
            0x244402 => Self::TRCSSCCR1,
            0x264402 => Self::TRCSSPCICR1,
            0x204404 => Self::TRCRSCTLR2,
            0x224404 => Self::TRCRSCTLR18,
            0x244404 => Self::TRCSSCCR2,
            0x264404 => Self::TRCSSPCICR2,
            0x204406 => Self::TRCRSCTLR3,
            0x224406 => Self::TRCRSCTLR19,
            0x244406 => Self::TRCSSCCR3,
            0x264406 => Self::TRCSSPCICR3,
            0x204408 => Self::TRCRSCTLR4,
            0x224408 => Self::TRCRSCTLR20,
            0x244408 => Self::TRCSSCCR4,
            0x264408 => Self::TRCSSPCICR4,
            0x284408 => Self::TRCPDCR,
            0x20440a => Self::TRCRSCTLR5,
            0x22440a => Self::TRCRSCTLR21,
            0x24440a => Self::TRCSSCCR5,
            0x26440a => Self::TRCSSPCICR5,
            0x20440c => Self::TRCRSCTLR6,
            0x22440c => Self::TRCRSCTLR22,
            0x24440c => Self::TRCSSCCR6,
            0x26440c => Self::TRCSSPCICR6,
            0x20440e => Self::TRCRSCTLR7,
            0x22440e => Self::TRCRSCTLR23,
            0x24440e => Self::TRCSSCCR7,
            0x26440e => Self::TRCSSPCICR7,
            0x204410 => Self::TRCRSCTLR8,
            0x224410 => Self::TRCRSCTLR24,
            0x244410 => Self::TRCSSCSR0,
            0x204412 => Self::TRCRSCTLR9,
            0x224412 => Self::TRCRSCTLR25,
            0x244412 => Self::TRCSSCSR1,
            0x204414 => Self::TRCRSCTLR10,
            0x224414 => Self::TRCRSCTLR26,
            0x244414 => Self::TRCSSCSR2,
            0x204416 => Self::TRCRSCTLR11,
            0x224416 => Self::TRCRSCTLR27,
            0x244416 => Self::TRCSSCSR3,
            0x204418 => Self::TRCRSCTLR12,
            0x224418 => Self::TRCRSCTLR28,
            0x244418 => Self::TRCSSCSR4,
            0x20441a => Self::TRCRSCTLR13,
            0x22441a => Self::TRCRSCTLR29,
            0x24441a => Self::TRCSSCSR5,
            0x20441c => Self::TRCRSCTLR14,
            0x22441c => Self::TRCRSCTLR30,
            0x24441c => Self::TRCSSCSR6,
            0x20441e => Self::TRCRSCTLR15,
            0x22441e => Self::TRCRSCTLR31,
            0x24441e => Self::TRCSSCSR7,
            0x204800 => Self::TRCACVR0,
            0x224800 => Self::TRCACVR8,
            0x244800 => Self::TRCACATR0,
            0x264800 => Self::TRCACATR8,
            0x284800 => Self::TRCDVCVR0,
            0x2a4800 => Self::TRCDVCVR4,
            0x2c4800 => Self::TRCDVCMR0,
            0x2e4800 => Self::TRCDVCMR4,
            0x204804 => Self::TRCACVR1,
            0x224804 => Self::TRCACVR9,
            0x244804 => Self::TRCACATR1,
            0x264804 => Self::TRCACATR9,
            0x204808 => Self::TRCACVR2,
            0x224808 => Self::TRCACVR10,
            0x244808 => Self::TRCACATR2,
            0x264808 => Self::TRCACATR10,
            0x284808 => Self::TRCDVCVR1,
            0x2a4808 => Self::TRCDVCVR5,
            0x2c4808 => Self::TRCDVCMR1,
            0x2e4808 => Self::TRCDVCMR5,
            0x20480c => Self::TRCACVR3,
            0x22480c => Self::TRCACVR11,
            0x24480c => Self::TRCACATR3,
            0x26480c => Self::TRCACATR11,
            0x204810 => Self::TRCACVR4,
            0x224810 => Self::TRCACVR12,
            0x244810 => Self::TRCACATR4,
            0x264810 => Self::TRCACATR12,
            0x284810 => Self::TRCDVCVR2,
            0x2a4810 => Self::TRCDVCVR6,
            0x2c4810 => Self::TRCDVCMR2,
            0x2e4810 => Self::TRCDVCMR6,
            0x204814 => Self::TRCACVR5,
            0x224814 => Self::TRCACVR13,
            0x244814 => Self::TRCACATR5,
            0x264814 => Self::TRCACATR13,
            0x204818 => Self::TRCACVR6,
            0x224818 => Self::TRCACVR14,
            0x244818 => Self::TRCACATR6,
            0x264818 => Self::TRCACATR14,
            0x284818 => Self::TRCDVCVR3,
            0x2a4818 => Self::TRCDVCVR7,
            0x2c4818 => Self::TRCDVCMR3,
            0x2e4818 => Self::TRCDVCMR7,
            0x20481c => Self::TRCACVR7,
            0x22481c => Self::TRCACVR15,
            0x24481c => Self::TRCACATR7,
            0x26481c => Self::TRCACATR15,
            0x204c00 => Self::TRCCIDCVR0,
            0x224c00 => Self::TRCVMIDCVR0,
            0x244c00 => Self::TRCCIDCCTLR0,
            0x244c02 => Self::TRCCIDCCTLR1,
            0x204c04 => Self::TRCCIDCVR1,
            0x224c04 => Self::TRCVMIDCVR1,
            0x244c04 => Self::TRCVMIDCCTLR0,
            0x244c06 => Self::TRCVMIDCCTLR1,
            0x204c08 => Self::TRCCIDCVR2,
            0x224c08 => Self::TRCVMIDCVR2,
            0x204c0c => Self::TRCCIDCVR3,
            0x224c0c => Self::TRCVMIDCVR3,
            0x204c10 => Self::TRCCIDCVR4,
            0x224c10 => Self::TRCVMIDCVR4,
            0x204c14 => Self::TRCCIDCVR5,
            0x224c14 => Self::TRCVMIDCVR5,
            0x204c18 => Self::TRCCIDCVR6,
            0x224c18 => Self::TRCVMIDCVR6,
            0x204c1c => Self::TRCCIDCVR7,
            0x224c1c => Self::TRCVMIDCVR7,
            0x285c00 => Self::TRCITCTRL,
            0x2c5c10 => Self::TRCCLAIMSET,
            0x2c5c12 => Self::TRCCLAIMCLR,
            0x2c5c18 => Self::TRCLAR,
            0x208000 => Self::TEECR32_EL1,
            0x208400 => Self::TEEHBR32_EL1,
            0x20c008 => Self::DBGDTR_EL0,
            0x20c00a => Self::DBGDTRTX_EL0,
            0x21000e => Self::DBGVCR32_EL2,
            0x300400 => Self::SCTLR_EL1,
            0x320400 => Self::ACTLR_EL1,
            0x340400 => Self::CPACR_EL1,
            0x3a0400 => Self::RGSR_EL1,
            0x3c0400 => Self::GCR_EL1,
            0x320404 => Self::TRFCR_EL1,
            0x300800 => Self::TTBR0_EL1,
            0x320800 => Self::TTBR1_EL1,
            0x340800 => Self::TCR_EL1,
            0x300802 => Self::APIAKEYLO_EL1,
            0x320802 => Self::APIAKEYHI_EL1,
            0x340802 => Self::APIBKEYLO_EL1,
            0x360802 => Self::APIBKEYHI_EL1,
            0x300804 => Self::APDAKEYLO_EL1,
            0x320804 => Self::APDAKEYHI_EL1,
            0x340804 => Self::APDBKEYLO_EL1,
            0x360804 => Self::APDBKEYHI_EL1,
            0x300806 => Self::APGAKEYLO_EL1,
            0x320806 => Self::APGAKEYHI_EL1,
            0x301000 => Self::SPSR_EL1,
            0x321000 => Self::ELR_EL1,
            0x301002 => Self::SP_EL0,
            0x301004 => Self::SPSEL,
            0x341004 => Self::CURRENTEL,
            0x361004 => Self::PAN,
            0x381004 => Self::UAO,
            0x30100c => Self::ICC_PMR_EL1,
            0x301402 => Self::AFSR0_EL1,
            0x321402 => Self::AFSR1_EL1,
            0x301404 => Self::ESR_EL1,
            0x321406 => Self::ERRSELR_EL1,
            0x321408 => Self::ERXCTLR_EL1,
            0x341408 => Self::ERXSTATUS_EL1,
            0x361408 => Self::ERXADDR_EL1,
            0x3a1408 => Self::ERXPFGCTL_EL1,
            0x3c1408 => Self::ERXPFGCDN_EL1,
            0x30140a => Self::ERXMISC0_EL1,
            0x32140a => Self::ERXMISC1_EL1,
            0x34140a => Self::ERXMISC2_EL1,
            0x36140a => Self::ERXMISC3_EL1,
            0x3e140a => Self::ERXTS_EL1,
            0x30140c => Self::TFSR_EL1,
            0x32140c => Self::TFSRE0_EL1,
            0x301800 => Self::FAR_EL1,
            0x301c08 => Self::PAR_EL1,
            0x302412 => Self::PMSCR_EL1,
            0x342412 => Self::PMSICR_EL1,
            0x362412 => Self::PMSIRR_EL1,
            0x382412 => Self::PMSFCR_EL1,
            0x3a2412 => Self::PMSEVFR_EL1,
            0x3c2412 => Self::PMSLATFR_EL1,
            0x3e2412 => Self::PMSIDR_EL1,
            0x302414 => Self::PMBLIMITR_EL1,
            0x322414 => Self::PMBPTR_EL1,
            0x362414 => Self::PMBSR_EL1,
            0x3e2414 => Self::PMBIDR_EL1,
            0x302416 => Self::TRBLIMITR_EL1,
            0x322416 => Self::TRBPTR_EL1,
            0x342416 => Self::TRBBASER_EL1,
            0x362416 => Self::TRBSR_EL1,
            0x382416 => Self::TRBMAR_EL1,
            0x3c2416 => Self::TRBTRG_EL1,
            0x32241c => Self::PMINTENSET_EL1,
            0x34241c => Self::PMINTENCLR_EL1,
            0x3c241c => Self::PMMIR_EL1,
            0x302804 => Self::MAIR_EL1,
            0x302806 => Self::AMAIR_EL1,
            0x302808 => Self::LORSA_EL1,
            0x322808 => Self::LOREA_EL1,
            0x342808 => Self::LORN_EL1,
            0x362808 => Self::LORC_EL1,
            0x30280a => Self::MPAM1_EL1,
            0x32280a => Self::MPAM0_EL1,
            0x303000 => Self::VBAR_EL1,
            0x343000 => Self::RMR_EL1,
            0x323002 => Self::DISR_EL1,
            0x323010 => Self::ICC_EOIR0_EL1,
            0x363010 => Self::ICC_BPR0_EL1,
            0x383010 => Self::ICC_AP0R0_EL1,
            0x3a3010 => Self::ICC_AP0R1_EL1,
            0x3c3010 => Self::ICC_AP0R2_EL1,
            0x3e3010 => Self::ICC_AP0R3_EL1,
            0x303012 => Self::ICC_AP1R0_EL1,
            0x323012 => Self::ICC_AP1R1_EL1,
            0x343012 => Self::ICC_AP1R2_EL1,
            0x363012 => Self::ICC_AP1R3_EL1,
            0x323016 => Self::ICC_DIR_EL1,
            0x3a3016 => Self::ICC_SGI1R_EL1,
            0x3c3016 => Self::ICC_ASGI1R_EL1,
            0x3e3016 => Self::ICC_SGI0R_EL1,
            0x323018 => Self::ICC_EOIR1_EL1,
            0x363018 => Self::ICC_BPR1_EL1,
            0x383018 => Self::ICC_CTLR_EL1,
            0x3a3018 => Self::ICC_SRE_EL1,
            0x3c3018 => Self::ICC_IGRPEN0_EL1,
            0x3e3018 => Self::ICC_IGRPEN1_EL1,
            0x30301a => Self::ICC_SEIEN_EL1,
            0x323400 => Self::CONTEXTIDR_EL1,
            0x383400 => Self::TPIDR_EL1,
            0x3e3400 => Self::SCXTNUM_EL1,
            0x303802 => Self::CNTKCTL_EL1,
            0x308000 => Self::CSSELR_EL1,
            0x30d004 => Self::NZCV,
            0x32d004 => Self::DAIFSET,
            0x3ad004 => Self::DIT,
            0x3cd004 => Self::SSBS,
            0x3ed004 => Self::TCO,
            0x30d008 => Self::FPCR,
            0x32d008 => Self::FPSR,
            0x30d00a => Self::DSPSR_EL0,
            0x32d00a => Self::DLR_EL0,
            0x30e418 => Self::PMCR_EL0,
            0x32e418 => Self::PMCNTENSET_EL0,
            0x34e418 => Self::PMCNTENCLR_EL0,
            0x36e418 => Self::PMOVSCLR_EL0,
            0x38e418 => Self::PMSWINC_EL0,
            0x3ae418 => Self::PMSELR_EL0,
            0x30e41a => Self::PMCCNTR_EL0,
            0x32e41a => Self::PMXEVTYPER_EL0,
            0x34e41a => Self::PMXEVCNTR_EL0,
            0x3ae41a => Self::DAIFCLR,
            0x30e41c => Self::PMUSERENR_EL0,
            0x36e41c => Self::PMOVSSET_EL0,
            0x34f400 => Self::TPIDR_EL0,
            0x36f400 => Self::TPIDRRO_EL0,
            0x3ef400 => Self::SCXTNUM_EL0,
            0x30f404 => Self::AMCR_EL0,
            0x36f404 => Self::AMUSERENR_EL0,
            0x38f404 => Self::AMCNTENCLR0_EL0,
            0x3af404 => Self::AMCNTENSET0_EL0,
            0x30f406 => Self::AMCNTENCLR1_EL0,
            0x32f406 => Self::AMCNTENSET1_EL0,
            0x30f408 => Self::AMEVCNTR00_EL0,
            0x32f408 => Self::AMEVCNTR01_EL0,
            0x34f408 => Self::AMEVCNTR02_EL0,
            0x36f408 => Self::AMEVCNTR03_EL0,
            0x30f418 => Self::AMEVCNTR10_EL0,
            0x32f418 => Self::AMEVCNTR11_EL0,
            0x34f418 => Self::AMEVCNTR12_EL0,
            0x36f418 => Self::AMEVCNTR13_EL0,
            0x38f418 => Self::AMEVCNTR14_EL0,
            0x3af418 => Self::AMEVCNTR15_EL0,
            0x3cf418 => Self::AMEVCNTR16_EL0,
            0x3ef418 => Self::AMEVCNTR17_EL0,
            0x30f41a => Self::AMEVCNTR18_EL0,
            0x32f41a => Self::AMEVCNTR19_EL0,
            0x34f41a => Self::AMEVCNTR110_EL0,
            0x36f41a => Self::AMEVCNTR111_EL0,
            0x38f41a => Self::AMEVCNTR112_EL0,
            0x3af41a => Self::AMEVCNTR113_EL0,
            0x3cf41a => Self::AMEVCNTR114_EL0,
            0x3ef41a => Self::AMEVCNTR115_EL0,
            0x30f41c => Self::AMEVTYPER10_EL0,
            0x32f41c => Self::AMEVTYPER11_EL0,
            0x34f41c => Self::AMEVTYPER12_EL0,
            0x36f41c => Self::AMEVTYPER13_EL0,
            0x38f41c => Self::AMEVTYPER14_EL0,
            0x3af41c => Self::AMEVTYPER15_EL0,
            0x3cf41c => Self::AMEVTYPER16_EL0,
            0x3ef41c => Self::AMEVTYPER17_EL0,
            0x30f41e => Self::AMEVTYPER18_EL0,
            0x32f41e => Self::AMEVTYPER19_EL0,
            0x34f41e => Self::AMEVTYPER110_EL0,
            0x36f41e => Self::AMEVTYPER111_EL0,
            0x38f41e => Self::AMEVTYPER112_EL0,
            0x3af41e => Self::AMEVTYPER113_EL0,
            0x3cf41e => Self::AMEVTYPER114_EL0,
            0x3ef41e => Self::AMEVTYPER115_EL0,
            0x30f800 => Self::CNTFRQ_EL0,
            0x32f800 => Self::CNTPCT_EL0,
            0x30f804 => Self::CNTP_TVAL_EL0,
            0x32f804 => Self::CNTP_CTL_EL0,
            0x34f804 => Self::CNTP_CVAL_EL0,
            0x30f806 => Self::CNTV_TVAL_EL0,
            0x32f806 => Self::CNTV_CTL_EL0,
            0x34f806 => Self::CNTV_CVAL_EL0,
            0x30f810 => Self::PMEVCNTR0_EL0,
            0x32f810 => Self::PMEVCNTR1_EL0,
            0x34f810 => Self::PMEVCNTR2_EL0,
            0x36f810 => Self::PMEVCNTR3_EL0,
            0x38f810 => Self::PMEVCNTR4_EL0,
            0x3af810 => Self::PMEVCNTR5_EL0,
            0x3cf810 => Self::PMEVCNTR6_EL0,
            0x3ef810 => Self::PMEVCNTR7_EL0,
            0x30f812 => Self::PMEVCNTR8_EL0,
            0x32f812 => Self::PMEVCNTR9_EL0,
            0x34f812 => Self::PMEVCNTR10_EL0,
            0x36f812 => Self::PMEVCNTR11_EL0,
            0x38f812 => Self::PMEVCNTR12_EL0,
            0x3af812 => Self::PMEVCNTR13_EL0,
            0x3cf812 => Self::PMEVCNTR14_EL0,
            0x3ef812 => Self::PMEVCNTR15_EL0,
            0x30f814 => Self::PMEVCNTR16_EL0,
            0x32f814 => Self::PMEVCNTR17_EL0,
            0x34f814 => Self::PMEVCNTR18_EL0,
            0x36f814 => Self::PMEVCNTR19_EL0,
            0x38f814 => Self::PMEVCNTR20_EL0,
            0x3af814 => Self::PMEVCNTR21_EL0,
            0x3cf814 => Self::PMEVCNTR22_EL0,
            0x3ef814 => Self::PMEVCNTR23_EL0,
            0x30f816 => Self::PMEVCNTR24_EL0,
            0x32f816 => Self::PMEVCNTR25_EL0,
            0x34f816 => Self::PMEVCNTR26_EL0,
            0x36f816 => Self::PMEVCNTR27_EL0,
            0x38f816 => Self::PMEVCNTR28_EL0,
            0x3af816 => Self::PMEVCNTR29_EL0,
            0x3cf816 => Self::PMEVCNTR30_EL0,
            0x30f818 => Self::PMEVTYPER0_EL0,
            0x32f818 => Self::PMEVTYPER1_EL0,
            0x34f818 => Self::PMEVTYPER2_EL0,
            0x36f818 => Self::PMEVTYPER3_EL0,
            0x38f818 => Self::PMEVTYPER4_EL0,
            0x3af818 => Self::PMEVTYPER5_EL0,
            0x3cf818 => Self::PMEVTYPER6_EL0,
            0x3ef818 => Self::PMEVTYPER7_EL0,
            0x30f81a => Self::PMEVTYPER8_EL0,
            0x32f81a => Self::PMEVTYPER9_EL0,
            0x34f81a => Self::PMEVTYPER10_EL0,
            0x36f81a => Self::PMEVTYPER11_EL0,
            0x38f81a => Self::PMEVTYPER12_EL0,
            0x3af81a => Self::PMEVTYPER13_EL0,
            0x3cf81a => Self::PMEVTYPER14_EL0,
            0x3ef81a => Self::PMEVTYPER15_EL0,
            0x30f81c => Self::PMEVTYPER16_EL0,
            0x32f81c => Self::PMEVTYPER17_EL0,
            0x34f81c => Self::PMEVTYPER18_EL0,
            0x36f81c => Self::PMEVTYPER19_EL0,
            0x38f81c => Self::PMEVTYPER20_EL0,
            0x3af81c => Self::PMEVTYPER21_EL0,
            0x3cf81c => Self::PMEVTYPER22_EL0,
            0x3ef81c => Self::PMEVTYPER23_EL0,
            0x30f81e => Self::PMEVTYPER24_EL0,
            0x32f81e => Self::PMEVTYPER25_EL0,
            0x34f81e => Self::PMEVTYPER26_EL0,
            0x36f81e => Self::PMEVTYPER27_EL0,
            0x38f81e => Self::PMEVTYPER28_EL0,
            0x3af81e => Self::PMEVTYPER29_EL0,
            0x3cf81e => Self::PMEVTYPER30_EL0,
            0x3ef81e => Self::PMCCFILTR_EL0,
            0x310000 => Self::VPIDR_EL2,
            0x3b0000 => Self::VMPIDR_EL2,
            0x310400 => Self::SCTLR_EL2,
            0x330400 => Self::ACTLR_EL2,
            0x310402 => Self::HCR_EL2,
            0x330402 => Self::MDCR_EL2,
            0x350402 => Self::CPTR_EL2,
            0x370402 => Self::HSTR_EL2,
            0x3f0402 => Self::HACR_EL2,
            0x330404 => Self::TRFCR_EL2,
            0x330406 => Self::SDER32_EL2,
            0x310800 => Self::TTBR0_EL2,
            0x330800 => Self::TTBR1_EL2,
            0x350800 => Self::TCR_EL2,
            0x310802 => Self::VTTBR_EL2,
            0x350802 => Self::VTCR_EL2,
            0x310804 => Self::VNCR_EL2,
            0x31080c => Self::VSTTBR_EL2,
            0x35080c => Self::VSTCR_EL2,
            0x310c00 => Self::DACR32_EL2,
            0x311000 => Self::SPSR_EL2,
            0x331000 => Self::ELR_EL2,
            0x311002 => Self::SP_EL1,
            0x311006 => Self::SPSR_IRQ,
            0x331006 => Self::SPSR_ABT,
            0x351006 => Self::SPSR_UND,
            0x371006 => Self::SPSR_FIQ,
            0x331400 => Self::IFSR32_EL2,
            0x311402 => Self::AFSR0_EL2,
            0x331402 => Self::AFSR1_EL2,
            0x311404 => Self::ESR_EL2,
            0x371404 => Self::VSESR_EL2,
            0x311406 => Self::FPEXC32_EL2,
            0x31140c => Self::TFSR_EL2,
            0x311800 => Self::FAR_EL2,
            0x391800 => Self::HPFAR_EL2,
            0x312412 => Self::PMSCR_EL2,
            0x312804 => Self::MAIR_EL2,
            0x312806 => Self::AMAIR_EL2,
            0x312808 => Self::MPAMHCR_EL2,
            0x332808 => Self::MPAMVPMV_EL2,
            0x31280a => Self::MPAM2_EL2,
            0x31280c => Self::MPAMVPM0_EL2,
            0x33280c => Self::MPAMVPM1_EL2,
            0x35280c => Self::MPAMVPM2_EL2,
            0x37280c => Self::MPAMVPM3_EL2,
            0x39280c => Self::MPAMVPM4_EL2,
            0x3b280c => Self::MPAMVPM5_EL2,
            0x3d280c => Self::MPAMVPM6_EL2,
            0x3f280c => Self::MPAMVPM7_EL2,
            0x313000 => Self::VBAR_EL2,
            0x353000 => Self::RMR_EL2,
            0x333002 => Self::VDISR_EL2,
            0x313010 => Self::ICH_AP0R0_EL2,
            0x333010 => Self::ICH_AP0R1_EL2,
            0x353010 => Self::ICH_AP0R2_EL2,
            0x373010 => Self::ICH_AP0R3_EL2,
            0x313012 => Self::ICH_AP1R0_EL2,
            0x333012 => Self::ICH_AP1R1_EL2,
            0x353012 => Self::ICH_AP1R2_EL2,
            0x373012 => Self::ICH_AP1R3_EL2,
            0x393012 => Self::ICH_VSEIR_EL2,
            0x3b3012 => Self::ICC_SRE_EL2,
            0x313016 => Self::ICH_HCR_EL2,
            0x353016 => Self::ICH_MISR_EL2,
            0x3f3016 => Self::ICH_VMCR_EL2,
            0x313018 => Self::ICH_LR0_EL2,
            0x333018 => Self::ICH_LR1_EL2,
            0x353018 => Self::ICH_LR2_EL2,
            0x373018 => Self::ICH_LR3_EL2,
            0x393018 => Self::ICH_LR4_EL2,
            0x3b3018 => Self::ICH_LR5_EL2,
            0x3d3018 => Self::ICH_LR6_EL2,
            0x3f3018 => Self::ICH_LR7_EL2,
            0x31301a => Self::ICH_LR8_EL2,
            0x33301a => Self::ICH_LR9_EL2,
            0x35301a => Self::ICH_LR10_EL2,
            0x37301a => Self::ICH_LR11_EL2,
            0x39301a => Self::ICH_LR12_EL2,
            0x3b301a => Self::ICH_LR13_EL2,
            0x3d301a => Self::ICH_LR14_EL2,
            0x3f301a => Self::ICH_LR15_EL2,
            0x333400 => Self::CONTEXTIDR_EL2,
            0x353400 => Self::TPIDR_EL2,
            0x3f3400 => Self::SCXTNUM_EL2,
            0x373800 => Self::CNTVOFF_EL2,
            0x313802 => Self::CNTHCTL_EL2,
            0x313804 => Self::CNTHP_TVAL_EL2,
            0x333804 => Self::CNTHP_CTL_EL2,
            0x353804 => Self::CNTHP_CVAL_EL2,
            0x313806 => Self::CNTHV_TVAL_EL2,
            0x333806 => Self::CNTHV_CTL_EL2,
            0x353806 => Self::CNTHV_CVAL_EL2,
            0x313808 => Self::CNTHVS_TVAL_EL2,
            0x333808 => Self::CNTHVS_CTL_EL2,
            0x353808 => Self::CNTHVS_CVAL_EL2,
            0x31380a => Self::CNTHPS_TVAL_EL2,
            0x33380a => Self::CNTHPS_CTL_EL2,
            0x35380a => Self::CNTHPS_CVAL_EL2,
            0x314400 => Self::SCTLR_EL12,
            0x354400 => Self::CPACR_EL12,
            0x334404 => Self::TRFCR_EL12,
            0x314800 => Self::TTBR0_EL12,
            0x334800 => Self::TTBR1_EL12,
            0x354800 => Self::TCR_EL12,
            0x315000 => Self::SPSR_EL12,
            0x335000 => Self::ELR_EL12,
            0x315402 => Self::AFSR0_EL12,
            0x335402 => Self::AFSR1_EL12,
            0x315404 => Self::ESR_EL12,
            0x31540c => Self::TFSR_EL12,
            0x315800 => Self::FAR_EL12,
            0x316412 => Self::PMSCR_EL12,
            0x316804 => Self::MAIR_EL12,
            0x316806 => Self::AMAIR_EL12,
            0x31680a => Self::MPAM1_EL12,
            0x317000 => Self::VBAR_EL12,
            0x337400 => Self::CONTEXTIDR_EL12,
            0x3f7400 => Self::SCXTNUM_EL12,
            0x317802 => Self::CNTKCTL_EL12,
            0x317804 => Self::CNTP_TVAL_EL02,
            0x337804 => Self::CNTP_CTL_EL02,
            0x357804 => Self::CNTP_CVAL_EL02,
            0x317806 => Self::CNTV_TVAL_EL02,
            0x337806 => Self::CNTV_CTL_EL02,
            0x357806 => Self::CNTV_CVAL_EL02,
            0x318400 => Self::SCTLR_EL3,
            0x338400 => Self::ACTLR_EL3,
            0x318402 => Self::SCR_EL3,
            0x338402 => Self::SDER32_EL3,
            0x358402 => Self::CPTR_EL3,
            0x338406 => Self::MDCR_EL3,
            0x318800 => Self::TTBR0_EL3,
            0x358800 => Self::TCR_EL3,
            0x319000 => Self::SPSR_EL3,
            0x339000 => Self::ELR_EL3,
            0x319002 => Self::SP_EL2,
            0x319402 => Self::AFSR0_EL3,
            0x339402 => Self::AFSR1_EL3,
            0x319404 => Self::ESR_EL3,
            0x31940c => Self::TFSR_EL3,
            0x319800 => Self::FAR_EL3,
            0x31a804 => Self::MAIR_EL3,
            0x31a806 => Self::AMAIR_EL3,
            0x31a80a => Self::MPAM3_EL3,
            0x31b000 => Self::VBAR_EL3,
            0x35b000 => Self::RMR_EL3,
            0x39b018 => Self::ICC_CTLR_EL3,
            0x3bb018 => Self::ICC_SRE_EL3,
            0x3fb018 => Self::ICC_IGRPEN1_EL3,
            0x35b400 => Self::TPIDR_EL3,
            0x3fb400 => Self::SCXTNUM_EL3,
            0x31f804 => Self::CNTPS_TVAL_EL1,
            0x33f804 => Self::CNTPS_CTL_EL1,
            0x35f804 => Self::CNTPS_CVAL_EL1,
            0x37f804 => Self::PSTATE_SPSEL,
            _ => panic!("Invalid system register value"),
        }
    }
}

impl LowerHex for SystemRegType {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        write!(f, "{:x}", *self as usize)
    }
}

impl UpperHex for SystemRegType {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        write!(f, "{:X}", *self as usize)
    }
}

// eMMC clock divider bit shift position
pub const CCLK_EMMC_DIV_SHIFT: u32 = 8;
// eMMC clock divider mask (6 bits, bits [13:8])
pub const CCLK_EMMC_DIV_MASK: u32 = 0x3f << CCLK_EMMC_DIV_SHIFT;
// eMMC clock source selector bit shift position
pub const CCLK_EMMC_SEL_SHIFT: u32 = 14;
// eMMC clock source selector mask (2 bits, bits [15:14])
pub const CCLK_EMMC_SEL_MASK: u32 = 3 << CCLK_EMMC_SEL_SHIFT;

// eMMC clock source: GPLL (General Purpose PLL)
pub const CCLK_EMMC_SEL_GPLL: u32 = 0;
// eMMC clock source: CPLL (Codec PLL)
pub const CCLK_EMMC_SEL_CPLL: u32 = 1;

// SFC (Serial Flash Controller) clock source: CPLL
pub const SCLK_SFC_SEL_CPLL: u32 = 0;
// SFC clock source: GPLL
pub const SCLK_SFC_SEL_GPLL: u32 = 1;
// SFC clock source: 24MHz oscillator
pub const SCLK_SFC_SEL_24M: u32 = 2;

// NPU (Neural Processing Unit) clocks
// NPU core 1 AXI bus clock
pub const ACLK_NPU1: u32 = 290;
// NPU core 1 AHB bus clock
pub const HCLK_NPU1: u32 = 291;
// NPU core 2 AXI bus clock
pub const ACLK_NPU2: u32 = 292;
// NPU core 2 AHB bus clock
pub const HCLK_NPU2: u32 = 293;
// NPU CM0 root AHB bus clock
pub const HCLK_NPU_CM0_ROOT: u32 = 294;
// NPU CM0 core function clock
pub const FCLK_NPU_CM0_CORE: u32 = 295;
// NPU CM0 RTC clock
pub const CLK_NPU_CM0_RTC: u32 = 296;
// NPU PVTM (Process, Voltage, Temperature Monitor) APB clock
pub const PCLK_NPU_PVTM: u32 = 297;
// NPU GRF (General Register Files) APB clock
pub const PCLK_NPU_GRF: u32 = 298;
// NPU PVTM monitor clock
pub const CLK_NPU_PVTM: u32 = 299;
// NPU core PVTM monitor clock
pub const CLK_CORE_NPU_PVTM: u32 = 300;
// NPU core 0 AXI bus clock
pub const ACLK_NPU0: u32 = 301;
// NPU core 0 AHB bus clock
pub const HCLK_NPU0: u32 = 302;
// NPU root AHB bus clock
pub const HCLK_NPU_ROOT: u32 = 303;
// NPU DSU0 (DynamIQ Shared Unit) clock
pub const CLK_NPU_DSU0: u32 = 304;
// NPU root APB bus clock
pub const PCLK_NPU_ROOT: u32 = 305;
// NPU timer APB clock
pub const PCLK_NPU_TIMER: u32 = 306;
// NPU timer root clock
pub const CLK_NPUTIMER_ROOT: u32 = 307;
// NPU timer 0 clock
pub const CLK_NPUTIMER0: u32 = 308;
// NPU timer 1 clock
pub const CLK_NPUTIMER1: u32 = 309;
// NPU watchdog timer APB clock
pub const PCLK_NPU_WDT: u32 = 310;
// NPU watchdog timer clock
pub const TCLK_NPU_WDT: u32 = 311;

// SD/eMMC/SFC storage device clocks
// DECOM (Decompression) display clock
pub const DCLK_DECOM: u32 = 119;
// eMMC controller core clock
pub const CCLK_EMMC: u32 = 314;
// eMMC bus interface clock
pub const BCLK_EMMC: u32 = 315;
// SFC (Serial Flash Controller) clock
pub const SCLK_SFC: u32 = 317;
// SDIO controller source clock
pub const CCLK_SRC_SDIO: u32 = 410;

// USB controller clocks
// USB3 DWC3 controller clocks (usbdrd3_0 @ 0xfc000000)
pub const CLK_REF_USB3OTG0: u32 = 0x1a3; // 419 - USB3 OTG0 reference clock
pub const CLK_SUSPEND_USB3OTG0: u32 = 0x1a2; // 418 - USB3 OTG0 suspend clock
pub const ACLK_USB3OTG0: u32 = 0x1a1; // 417 - USB3 OTG0 bus clock

// USB3 DWC3 controller clocks (usbdrd3_1 @ 0xfc400000)
pub const CLK_REF_USB3OTG1: u32 = 0x1a6; // 422 - USB3 OTG1 reference clock
pub const CLK_SUSPEND_USB3OTG1: u32 = 0x1a5; // 421 - USB3 OTG1 suspend clock
pub const ACLK_USB3OTG1: u32 = 0x1a4; // 420 - USB3 OTG1 bus clock

// USB3 Host controller clocks (usbhost3_0 @ 0xfcd00000)
pub const CLK_REF_USBHOST3_0: u32 = 0x179; // 377 - USB3 Host reference clock
pub const CLK_SUSPEND_USBHOST3_0: u32 = 0x178; // 376 - USB3 Host suspend clock
pub const ACLK_USBHOST3_0: u32 = 0x177; // 375 - USB3 Host bus clock
pub const CLK_UTMI_USBHOST3_0: u32 = 0x17a; // 378 - USB3 Host UTMI clock
pub const CLK_PIPE_USBHOST3_0: u32 = 0x181; // 385 - USB3 Host PIPE clock
pub const PCLK_PHP_USBHOST3_0: u32 = 0x166; // 358 - USB3 Host PHP clock

// USB2 EHCI/OHCI Host controller clocks (@ 0xfc800000/0xfc840000)
pub const CLK_USBHOST0: u32 = 0x19d; // 413 - USB2 Host0 controller clock
pub const CLK_USBHOST0_ARB: u32 = 0x19e; // 414 - USB2 Host0 arbiter clock

// USB2 EHCI/OHCI Host controller clocks (@ 0xfc880000/0xfc8c0000)
pub const CLK_USBHOST1: u32 = 0x19f; // 415 - USB2 Host1 controller clock
pub const CLK_USBHOST1_ARB: u32 = 0x1a0; // 416 - USB2 Host1 arbiter clock

// USB2 PHY clocks
pub const CLK_USBPHY_480M: u32 = 0x2b5; // 693 - USB PHY 480MHz clock

// USB bus clocks (gate control)
pub const ACLK_USB: u32 = 0x263; // 611 - USB AXI bus clock
pub const HCLK_USB: u32 = 0x264; // 612 - USB AHB bus clock

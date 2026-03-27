//! FXMAC hardware register offsets and bit definitions.
//!
//! This module mirrors the low-level register layout from the FXMAC hardware
//! specification and is primarily intended for internal driver use.
////////////////////
// fxmac_hw.h

pub(crate) const FXMAC_RX_BUF_UNIT: u32 = 64; /* Number of receive buffer bytes as a unit, this is HW setup */

pub(crate) const FXMAC_MAX_RXBD: u32 = 128; /* Size of RX buffer descriptor queues */
pub(crate) const FXMAC_MAX_TXBD: u32 = 128; /* Size of TX buffer descriptor queues */

pub(crate) const FXMAC_MAX_HASH_BITS: u32 = 64; /* Maximum value for hash bits. 2**6 */

/************************** Constant Definitions *****************************/

pub(crate) const FXMAC_MAX_MAC_ADDR: u32 = 4; /* Maxmum number of mac address supported */
pub(crate) const FXMAC_MAX_TYPE_ID: u32 = 4; /* Maxmum number of type id supported */

/// for aarch64
pub(crate) const FXMAC_BD_ALIGNMENT: u32 = 64; /* Minimum buffer descriptor alignment on the local bus */

pub(crate) const FXMAC_RX_BUF_ALIGNMENT: u32 = 4; /* Minimum buffer alignment when using options that impose
                                                  alignment  restrictions on the buffer data on the local bus */

pub(crate) const FXMAC_NWCTRL_OFFSET: u64 = 0x00000000; /* Network Control reg */
pub(crate) const FXMAC_NWCFG_OFFSET: u64 = 0x00000004; /* Network Config reg */
pub(crate) const FXMAC_NWSR_OFFSET: u64 = 0x00000008; /* Network Status reg */
pub(crate) const FXMAC_DMACR_OFFSET: u64 = 0x00000010; /* DMA Control reg */
pub(crate) const FXMAC_TXSR_OFFSET: u64 = 0x00000014; /* TX Status reg */
pub(crate) const FXMAC_RXQBASE_OFFSET: u64 = 0x00000018; /* RX Q Base address reg */
pub(crate) const FXMAC_TXQBASE_OFFSET: u64 = 0x0000001C; /* TX Q Base address reg */
pub(crate) const FXMAC_RXSR_OFFSET: u64 = 0x00000020; /* RX Status reg */

pub(crate) const FXMAC_ISR_OFFSET: u64 = 0x00000024; /* Interrupt Status reg */
pub(crate) const FXMAC_IER_OFFSET: u64 = 0x00000028; /* Interrupt Enable reg */
pub(crate) const FXMAC_IDR_OFFSET: u64 = 0x0000002C; /* Interrupt Disable reg */
pub(crate) const FXMAC_IMR_OFFSET: u64 = 0x00000030; /* Interrupt Mask reg */

pub(crate) const FXMAC_PHYMNTNC_OFFSET: u64 = 0x00000034; /* Phy Maintaince reg */
pub(crate) const FXMAC_RXPAUSE_OFFSET: u64 = 0x00000038; /* RX Pause Time reg */
pub(crate) const FXMAC_TXPAUSE_OFFSET: u64 = 0x0000003C; /* TX Pause Time reg */

pub(crate) const FXMAC_JUMBOMAXLEN_OFFSET: u64 = 0x00000048; /* Jumbo max length reg */
pub(crate) const FXMAC_GEM_HSMAC: u32 = 0x0050; /* Hs mac config register*/
pub(crate) const FXMAC_RXWATERMARK_OFFSET: u64 = 0x0000007C; /* RX watermark reg */

pub(crate) const FXMAC_HASHL_OFFSET: u64 = 0x00000080; /* Hash Low address reg */
pub(crate) const FXMAC_HASHH_OFFSET: u64 = 0x00000084; /* Hash High address reg */

pub(crate) const FXMAC_GEM_SA1B: u32 = 0x0088; /* Specific1 Bottom */
pub(crate) const FXMAC_GEM_SA1T: u32 = 0x008C; /* Specific1 Top */
pub(crate) const FXMAC_GEM_SA2B: u32 = 0x0090; /* Specific2 Bottom */
pub(crate) const FXMAC_GEM_SA2T: u32 = 0x0094; /* Specific2 Top */
pub(crate) const FXMAC_GEM_SA3B: u32 = 0x0098; /* Specific3 Bottom */
pub(crate) const FXMAC_GEM_SA3T: u32 = 0x009C; /* Specific3 Top */
pub(crate) const FXMAC_GEM_SA4B: u32 = 0x00A0; /* Specific4 Bottom */
pub(crate) const FXMAC_GEM_SA4T: u32 = 0x00A4; /* Specific4 Top */

pub(crate) const FXMAC_MATCH1_OFFSET: u64 = 0x000000A8; /* Type ID1 Match reg */
pub(crate) const FXMAC_MATCH2_OFFSET: u64 = 0x000000AC; /* Type ID2 Match reg */
pub(crate) const FXMAC_MATCH3_OFFSET: u64 = 0x000000B0; /* Type ID3 Match reg */
pub(crate) const FXMAC_MATCH4_OFFSET: u64 = 0x000000B4; /* Type ID4 Match reg */

pub(crate) const FXMAC_STRETCH_OFFSET: u64 = 0x000000BC; /* IPG Stretch reg */
pub(crate) const FXMAC_REVISION_REG_OFFSET: u64 = 0x000000FC; /*   identification number and module revision */

pub(crate) const FXMAC_OCTTXL_OFFSET: u64 = 0x00000100; /* Octects transmitted Low reg */
pub(crate) const FXMAC_OCTTXH_OFFSET: u64 = 0x00000104; /* Octects transmitted High reg */

pub(crate) const FXMAC_TXCNT_OFFSET: u64 = 0x00000108; /* Error-free Frmaes transmitted counter */
pub(crate) const FXMAC_TXBCCNT_OFFSET: u64 = 0x0000010C; /* Error-free Broadcast Frames counter*/
pub(crate) const FXMAC_TXMCCNT_OFFSET: u64 = 0x00000110; /* Error-free Multicast Frame counter */
pub(crate) const FXMAC_TXPAUSECNT_OFFSET: u64 = 0x00000114; /* Pause Frames Transmitted Counter */
pub(crate) const FXMAC_TX64CNT_OFFSET: u64 = 0x00000118; /* Error-free 64 byte Frames Transmitted counter */
pub(crate) const FXMAC_TX65CNT_OFFSET: u64 = 0x0000011C; /* Error-free 65-127 byte Frames Transmitted counter */
pub(crate) const FXMAC_TX128CNT_OFFSET: u64 = 0x00000120; /* Error-free 128-255 byte Frames Transmitted counter*/
pub(crate) const FXMAC_TX256CNT_OFFSET: u64 = 0x00000124; /* Error-free 256-511 byte Frames transmitted counter */
pub(crate) const FXMAC_TX512CNT_OFFSET: u64 = 0x00000128; /* Error-free 512-1023 byte Frames transmitted counter */
pub(crate) const FXMAC_TX1024CNT_OFFSET: u64 = 0x0000012C; /* Error-free 1024-1518 byte Frames transmitted counter */
pub(crate) const FXMAC_TX1519CNT_OFFSET: u64 = 0x00000130; /* Error-free larger than 1519 byte Frames transmitted counter */
pub(crate) const FXMAC_TXURUNCNT_OFFSET: u64 = 0x00000134; /* TX under run error counter */

pub(crate) const FXMAC_SNGLCOLLCNT_OFFSET: u64 = 0x00000138; /* Single Collision Frame Counter */
pub(crate) const FXMAC_MULTICOLLCNT_OFFSET: u64 = 0x0000013C; /* Multiple Collision Frame Counter */
pub(crate) const FXMAC_EXCESSCOLLCNT_OFFSET: u64 = 0x00000140; /* Excessive Collision Frame Counter */
pub(crate) const FXMAC_LATECOLLCNT_OFFSET: u64 = 0x00000144; /* Late Collision Frame Counter */
pub(crate) const FXMAC_TXDEFERCNT_OFFSET: u64 = 0x00000148; /* Deferred Transmission Frame Counter */
pub(crate) const FXMAC_TXCSENSECNT_OFFSET: u64 = 0x0000014C; /* Transmit Carrier Sense Error Counter */

pub(crate) const FXMAC_OCTRXL_OFFSET: u64 = 0x00000150; /* Octects Received register Low */
pub(crate) const FXMAC_OCTRXH_OFFSET: u64 = 0x00000154; /* Octects Received register High */

pub(crate) const FXMAC_RXCNT_OFFSET: u64 = 0x00000158; /* Error-free Frames Received Counter */
pub(crate) const FXMAC_RXBROADCNT_OFFSET: u64 = 0x0000015C; /* Error-free Broadcast Frames Received Counter */
pub(crate) const FXMAC_RXMULTICNT_OFFSET: u64 = 0x00000160; /* Error-free Multicast Frames Received Counter */
pub(crate) const FXMAC_RXPAUSECNT_OFFSET: u64 = 0x00000164; /* Pause Frames Received Counter */
pub(crate) const FXMAC_RX64CNT_OFFSET: u64 = 0x00000168; /* Error-free 64 byte Frames Received Counter */
pub(crate) const FXMAC_RX65CNT_OFFSET: u64 = 0x0000016C; /* Error-free 65-127 byte Frames Received Counter */
pub(crate) const FXMAC_RX128CNT_OFFSET: u64 = 0x00000170; /* Error-free 128-255 byte Frames Received Counter */
pub(crate) const FXMAC_RX256CNT_OFFSET: u64 = 0x00000174; /* Error-free 256-512 byte Frames Received Counter */
pub(crate) const FXMAC_RX512CNT_OFFSET: u64 = 0x00000178; /* Error-free 512-1023 byte Frames Received Counter */
pub(crate) const FXMAC_RX1024CNT_OFFSET: u64 = 0x0000017C; /* Error-free 1024-1518 byte Frames Received Counter */
pub(crate) const FXMAC_RX1519CNT_OFFSET: u64 = 0x00000180; /* Error-free 1519-max byte Frames Received Counter */
pub(crate) const FXMAC_RXUNDRCNT_OFFSET: u64 = 0x00000184; /* Undersize Frames Received Counter */
pub(crate) const FXMAC_RXOVRCNT_OFFSET: u64 = 0x00000188; /* Oversize Frames Received Counter */
pub(crate) const FXMAC_RXJABCNT_OFFSET: u64 = 0x0000018C; /* Jabbers Received Counter */
pub(crate) const FXMAC_RXFCSCNT_OFFSET: u64 = 0x00000190; /* Frame Check Sequence Error Counter */
pub(crate) const FXMAC_RXLENGTHCNT_OFFSET: u64 = 0x00000194; /* Length Field Error Counter */
pub(crate) const FXMAC_RXSYMBCNT_OFFSET: u64 = 0x00000198; /* Symbol Error Counter */
pub(crate) const FXMAC_RXALIGNCNT_OFFSET: u64 = 0x0000019C; /* Alignment Error Counter */
pub(crate) const FXMAC_RXRESERRCNT_OFFSET: u64 = 0x000001A0; /* Receive Resource Error Counter */
pub(crate) const FXMAC_RXORCNT_OFFSET: u64 = 0x000001A4; /* Receive Overrun Counter */
pub(crate) const FXMAC_RXIPCCNT_OFFSET: u64 = 0x000001A8; /* IP header Checksum Error Counter */
pub(crate) const FXMAC_RXTCPCCNT_OFFSET: u64 = 0x000001AC; /* TCP Checksum Error Counter */
pub(crate) const FXMAC_RXUDPCCNT_OFFSET: u64 = 0x000001B0; /* UDP Checksum Error Counter */
pub(crate) const FXMAC_LAST_OFFSET: u64 = 0x000001B4; /* Last statistic counter offset, for clearing */

pub(crate) const FXMAC_1588_SEC_OFFSET: u64 = 0x000001D0; /* 1588 second counter */
pub(crate) const FXMAC_1588_NANOSEC_OFFSET: u64 = 0x000001D4; /* 1588 nanosecond counter */
pub(crate) const FXMAC_1588_ADJ_OFFSET: u64 = 0x000001D8; /* 1588 nanosecond adjustment counter */
pub(crate) const FXMAC_1588_INC_OFFSET: u64 = 0x000001DC; /* 1588 nanosecond increment counter */
pub(crate) const FXMAC_PTP_TXSEC_OFFSET: u64 = 0x000001E0; /* 1588 PTP transmit second counter */
pub(crate) const FXMAC_PTP_TXNANOSEC_OFFSET: u64 = 0x000001E4; /* 1588 PTP transmit nanosecond counter */
pub(crate) const FXMAC_PTP_RXSEC_OFFSET: u64 = 0x000001E8; /* 1588 PTP receive second counter */
pub(crate) const FXMAC_PTP_RXNANOSEC_OFFSET: u64 = 0x000001EC; /* 1588 PTP receive nanosecond counter */
pub(crate) const FXMAC_PTPP_TXSEC_OFFSET: u64 = 0x000001F0; /* 1588 PTP peer transmit second counter */
pub(crate) const FXMAC_PTPP_TXNANOSEC_OFFSET: u64 = 0x000001F4; /* 1588 PTP peer transmit nanosecond counter */
pub(crate) const FXMAC_PTPP_RXSEC_OFFSET: u64 = 0x000001F8; /* 1588 PTP peer receive second counter */
pub(crate) const FXMAC_PTPP_RXNANOSEC_OFFSET: u64 = 0x000001FC; /* 1588 PTP peer receive nanosecond counter */

pub(crate) const FXMAC_PCS_CONTROL_OFFSET: u64 = 0x00000200; /* All PCS registers */

pub(crate) const FXMAC_PCS_STATUS_OFFSET: u64 = 0x00000204; /* All PCS status */

pub(crate) const FXMAC_PCS_AN_LP_OFFSET: u64 = 0x00000214; /* All PCS link partner's base page */

pub(crate) const FXMAC_DESIGNCFG_DEBUG1_OFFSET: u64 = 0x00000280; /* Design Configuration Register 1 */

pub(crate) const FXMAC_DESIGNCFG_DEBUG2_OFFSET: u64 = 0x00000284; /* Design Configuration Register 2 */

pub(crate) const FXMAC_INTQ1_STS_OFFSET: u64 = 0x00000400; /* Interrupt Q1 Status reg */

pub(crate) const FXMAC_TXQ1BASE_OFFSET: u64 = 0x00000440; /* TX Q1 Base address reg */
pub(crate) const FXMAC_RXQ1BASE_OFFSET: u64 = 0x00000480; /* RX Q1 Base address reg */

pub(crate) const FXMAC_RXBUFQ1_SIZE_OFFSET: u64 = 0x000004a0; /* Receive Buffer Size */
// FXMAC_RXBUFQX_SIZE_OFFSET(x) (FXMAC_RXBUFQ1_SIZE_OFFSET + (x << 2))
pub const fn FXMAC_RXBUFQX_SIZE_OFFSET(value: u64) -> u64 {
    FXMAC_RXBUFQ1_SIZE_OFFSET + (value << 2)
}
pub(crate) const FXMAC_RXBUFQX_SIZE_MASK: u32 = GENMASK(7, 0);

pub(crate) const FXMAC_MSBBUF_TXQBASE_OFFSET: u64 = 0x000004C8; /* MSB Buffer TX Q Base reg */
pub(crate) const FXMAC_MSBBUF_RXQBASE_OFFSET: u64 = 0x000004D4; /* MSB Buffer RX Q Base reg */
pub(crate) const FXMAC_TXQSEGALLOC_QLOWER_OFFSET: u64 = 0x000005A0; /* Transmit SRAM segment distribution */
pub(crate) const FXMAC_INTQ1_IER_OFFSET: u64 = 0x00000600; /* Interrupt Q1 Enable reg */
pub const fn FXMAC_INTQX_IER_SIZE_OFFSET(x: u64) -> u64 {
    FXMAC_INTQ1_IER_OFFSET + (x << 2)
}

pub(crate) const FXMAC_INTQ1_IDR_OFFSET: u64 = 0x00000620; /* Interrupt Q1 Disable reg */
pub const fn FXMAC_INTQX_IDR_SIZE_OFFSET(x: u64) -> u64 {
    FXMAC_INTQ1_IDR_OFFSET + (x << 2)
}

pub(crate) const FXMAC_INTQ1_IMR_OFFSET: u64 = 0x00000640; /* Interrupt Q1 Mask reg */

pub(crate) const FXMAC_GEM_USX_CONTROL_OFFSET: u64 = 0x0A80; /* High speed PCS control register */
pub(crate) const FXMAC_TEST_CONTROL_OFFSET: u64 = 0x0A84; /* USXGMII Test Control Register */
pub(crate) const FXMAC_GEM_USX_STATUS_OFFSET: u64 = 0x0A88; /* USXGMII Status Register */

pub(crate) const FXMAC_GEM_SRC_SEL_LN: u32 = 0x1C04;
pub(crate) const FXMAC_GEM_DIV_SEL0_LN: u32 = 0x1C08;
pub(crate) const FXMAC_GEM_DIV_SEL1_LN: u32 = 0x1C0C;
pub(crate) const FXMAC_GEM_PMA_XCVR_POWER_STATE: u32 = 0x1C10;
pub(crate) const FXMAC_GEM_SPEED_MODE: u32 = 0x1C14;
pub(crate) const FXMAC_GEM_MII_SELECT: u32 = 0x1C18;
pub(crate) const FXMAC_GEM_SEL_MII_ON_RGMII: u32 = 0x1C1C;
pub(crate) const FXMAC_GEM_TX_CLK_SEL0: u32 = 0x1C20;
pub(crate) const FXMAC_GEM_TX_CLK_SEL1: u32 = 0x1C24;
pub(crate) const FXMAC_GEM_TX_CLK_SEL2: u32 = 0x1C28;
pub(crate) const FXMAC_GEM_TX_CLK_SEL3: u32 = 0x1C2C;
pub(crate) const FXMAC_GEM_RX_CLK_SEL0: u32 = 0x1C30;
pub(crate) const FXMAC_GEM_RX_CLK_SEL1: u32 = 0x1C34;
pub(crate) const FXMAC_GEM_CLK_250M_DIV10_DIV100_SEL: u32 = 0x1C38;
pub(crate) const FXMAC_GEM_TX_CLK_SEL5: u32 = 0x1C3C;
pub(crate) const FXMAC_GEM_TX_CLK_SEL6: u32 = 0x1C40;
pub(crate) const FXMAC_GEM_RX_CLK_SEL4: u32 = 0x1C44;
pub(crate) const FXMAC_GEM_RX_CLK_SEL5: u32 = 0x1C48;
pub(crate) const FXMAC_GEM_TX_CLK_SEL3_0: u32 = 0x1C70;
pub(crate) const FXMAC_GEM_TX_CLK_SEL4_0: u32 = 0x1C74;
pub(crate) const FXMAC_GEM_RX_CLK_SEL3_0: u32 = 0x1C78;
pub(crate) const FXMAC_GEM_RX_CLK_SEL4_0: u32 = 0x1C7C;
pub(crate) const FXMAC_GEM_RGMII_TX_CLK_SEL0: u32 = 0x1C80;
pub(crate) const FXMAC_GEM_RGMII_TX_CLK_SEL1: u32 = 0x1C84;
pub(crate) const FXMAC_GEM_MODE_SEL_OFFSET: u64 = 0xDC00;
pub(crate) const FXMAC_LOOPBACK_SEL_OFFSET: u64 = 0xDC04;

pub(crate) const FXMAC_TAIL_ENABLE: u64 = 0xe7c; /*Enable tail Register*/
// FXMAC_TAIL_QUEUE(queue)		(0x0e80 + ((queue) << 2))

/**
 * @name interrupts bit definitions
 * Bits definitions are same in FXMAC_ISR_OFFSET,
 * FXMAC_IER_OFFSET, FXMAC_IDR_OFFSET, and FXMAC_IMR_OFFSET
 * @{
 */
pub(crate) const FXMAC_IXR_PTPPSTX_MASK: u32 = BIT(25); /* PTP Pdelay_resp TXed */
pub(crate) const FXMAC_IXR_PTPPDRTX_MASK: u32 = BIT(24); /* PTP Pdelay_req TXed */
pub(crate) const FXMAC_IXR_PTPPSRX_MASK: u32 = BIT(23); /* PTP Pdelay_resp RXed */
pub(crate) const FXMAC_IXR_PTPPDRRX_MASK: u32 = BIT(22); /* PTP Pdelay_req RXed */

pub(crate) const FXMAC_IXR_PTPSTX_MASK: u32 = BIT(21); /* PTP Sync TXed */
pub(crate) const FXMAC_IXR_PTPDRTX_MASK: u32 = BIT(20); /* PTP Delay_req TXed */
pub(crate) const FXMAC_IXR_PTPSRX_MASK: u32 = BIT(19); /* PTP Sync RXed */
pub(crate) const FXMAC_IXR_PTPDRRX_MASK: u32 = BIT(18); /* PTP Delay_req RXed */

pub(crate) const FXMAC_IXR_PAUSETX_MASK: u32 = BIT(14); /* Pause frame transmitted */
pub(crate) const FXMAC_IXR_PAUSEZERO_MASK: u32 = BIT(13); /* Pause time has reached zero */
pub(crate) const FXMAC_IXR_PAUSENZERO_MASK: u32 = BIT(12); /* Pause frame received */
pub(crate) const FXMAC_IXR_HRESPNOK_MASK: u32 = BIT(11); /* hresp not ok */
pub(crate) const FXMAC_IXR_RXOVR_MASK: u32 = BIT(10); /* Receive overrun occurred */
pub(crate) const FXMAC_IXR_LINKCHANGE_MASK: u32 = BIT(9); /* link status change */
pub(crate) const FXMAC_IXR_TXCOMPL_MASK: u32 = BIT(7); /* Frame transmitted ok */
pub(crate) const FXMAC_IXR_TXEXH_MASK: u32 = BIT(6); /* Transmit err occurred or no buffers*/
pub(crate) const FXMAC_IXR_RETRY_MASK: u32 = BIT(5); /* Retry limit exceeded */
pub(crate) const FXMAC_IXR_URUN_MASK: u32 = BIT(4); /* Transmit underrun */
pub(crate) const FXMAC_IXR_TXUSED_MASK: u32 = BIT(3); /* Tx buffer used bit read */
pub(crate) const FXMAC_IXR_RXUSED_MASK: u32 = BIT(2); /* Rx buffer used bit read */
pub(crate) const FXMAC_IXR_RXCOMPL_MASK: u32 = BIT(1); /* Frame received ok */
pub(crate) const FXMAC_IXR_MGMNT_MASK: u32 = BIT(0); /* PHY management complete */
pub(crate) const FXMAC_IXR_ALL_MASK: u32 = GENMASK(31, 0); /* Everything! */

pub(crate) const FXMAC_IXR_TX_ERR_MASK: u32 =
    (FXMAC_IXR_TXEXH_MASK | FXMAC_IXR_RETRY_MASK | FXMAC_IXR_URUN_MASK);

pub(crate) const FXMAC_IXR_RX_ERR_MASK: u32 =
    (FXMAC_IXR_HRESPNOK_MASK | FXMAC_IXR_RXUSED_MASK | FXMAC_IXR_RXOVR_MASK);

pub(crate) const FXMAC_INTR_MASK: u32 = (FXMAC_IXR_LINKCHANGE_MASK
    | FXMAC_IXR_TX_ERR_MASK
    | FXMAC_IXR_RX_ERR_MASK
    | FXMAC_IXR_RXCOMPL_MASK
    | FXMAC_IXR_TXCOMPL_MASK);

/** @name network control register bit definitions
 * @{
 */
pub(crate) const FXMAC_NWCTRL_ENABLE_HS_MAC_MASK: u32 = BIT(31);

pub(crate) const FXMAC_NWCTRL_TWO_PT_FIVE_GIG_MASK: u32 = BIT(29); /* 2.5G operation selected */

pub(crate) const FXMAC_NWCTRL_FLUSH_DPRAM_MASK: u32 = BIT(18); /* Flush a packet from Rx SRAM */
pub(crate) const FXMAC_NWCTRL_ZEROPAUSETX_MASK: u32 = BIT(11); /* Transmit zero quantum pause frame */
pub(crate) const FXMAC_NWCTRL_PAUSETX_MASK: u32 = BIT(11); /* Transmit pause frame */
pub(crate) const FXMAC_NWCTRL_HALTTX_MASK: u32 = BIT(10); /* Halt transmission after current frame */
pub(crate) const FXMAC_NWCTRL_STARTTX_MASK: u32 = BIT(9); /* Start tx (tx_go) */

pub(crate) const FXMAC_NWCTRL_STATWEN_MASK: u32 = BIT(7); /* Enable writing to stat counters */
pub(crate) const FXMAC_NWCTRL_STATINC_MASK: u32 = BIT(6); /* Increment statistic registers */
pub(crate) const FXMAC_NWCTRL_STATCLR_MASK: u32 = BIT(5); /* Clear statistic registers */
pub(crate) const FXMAC_NWCTRL_MDEN_MASK: u32 = BIT(4); /* Enable MDIO port */
pub(crate) const FXMAC_NWCTRL_TXEN_MASK: u32 = BIT(3); /* Enable transmit */
pub(crate) const FXMAC_NWCTRL_RXEN_MASK: u32 = BIT(2); /* Enable receive */
pub(crate) const FXMAC_NWCTRL_LOOPBACK_LOCAL_MASK: u32 = BIT(1); /* Loopback local */

/** @name network configuration register bit definitions FXMAC_NWCFG_OFFSET
 * @{
 */
pub(crate) const FXMAC_NWCFG_BADPREAMBEN_MASK: u32 = BIT(29); /* disable rejection of non-standard preamble */
pub(crate) const FXMAC_NWCFG_IPDSTRETCH_MASK: u32 = BIT(28); /* enable transmit IPG */
pub(crate) const FXMAC_NWCFG_SGMII_MODE_ENABLE_MASK: u32 = BIT(27); /* SGMII mode enable */
pub(crate) const FXMAC_NWCFG_FCSIGNORE_MASK: u32 = BIT(26); /* disable rejection of FCS error */
pub(crate) const FXMAC_NWCFG_HDRXEN_MASK: u32 = BIT(25); /* RX half duplex */
pub(crate) const FXMAC_NWCFG_RXCHKSUMEN_MASK: u32 = BIT(24); /* enable RX checksum offload */
pub(crate) const FXMAC_NWCFG_PAUSECOPYDI_MASK: u32 = BIT(23); /* Do not copy pause Frames to memory */

pub(crate) const FXMAC_NWCFG_DWIDTH_64_MASK: u32 = BIT(21); /* 64 bit Data bus width */
pub(crate) const FXMAC_NWCFG_BUS_WIDTH_32_MASK: u32 = (0 << 21);
pub(crate) const FXMAC_NWCFG_BUS_WIDTH_64_MASK: u32 = (1 << 21);
pub(crate) const FXMAC_NWCFG_BUS_WIDTH_128_MASK: u32 = (2 << 21);

pub(crate) const FXMAC_NWCFG_CLOCK_DIV224_MASK: u32 = (7 << 18);
pub(crate) const FXMAC_NWCFG_CLOCK_DIV128_MASK: u32 = (6 << 18);
pub(crate) const FXMAC_NWCFG_CLOCK_DIV96_MASK: u32 = (5 << 18);
pub(crate) const FXMAC_NWCFG_CLOCK_DIV64_MASK: u32 = (4 << 18);
pub(crate) const FXMAC_NWCFG_CLOCK_DIV48_MASK: u32 = (3 << 18);
pub(crate) const FXMAC_NWCFG_CLOCK_DIV32_MASK: u32 = (2 << 18);
pub(crate) const FXMAC_NWCFG_CLOCK_DIV16_MASK: u32 = (1 << 18);
pub(crate) const FXMAC_NWCFG_CLOCK_DIV8_MASK: u32 = (0 << 18);
pub(crate) const FXMAC_NWCFG_RESET_MASK: u32 = BIT(19); /* reset value of mdc_clock_division*/
pub(crate) const FXMAC_NWCFG_MDC_SHIFT_MASK: u32 = 18; /* shift bits for MDC */
pub(crate) const FXMAC_NWCFG_MDCCLKDIV_MASK: u32 = GENMASK(20, 18); /* MDC Mask PCLK divisor */

pub(crate) const FXMAC_NWCFG_FCS_REMOVE_MASK: u32 = BIT(17); /* FCS remove - setting this bit will cause received frames to be written to memory without their frame check sequence (last 4 bytes). */
pub(crate) const FXMAC_NWCFG_LENGTH_FIELD_ERROR_FRAME_DISCARD_MASK: u32 = BIT(16); /* RX length error discard */
// FXMAC_NWCFG_RXOFFS_MASK:u32 = GENMASK(15);  /* RX buffer offset */
pub(crate) const FXMAC_NWCFG_PAUSE_ENABLE_MASK: u32 = BIT(13); /* Pause enable - when set, transmission will pause if a non-zero 802.3 classic pause frame is received and PFC has not been negotiated. */
pub(crate) const FXMAC_NWCFG_RETRYTESTEN_MASK: u32 = BIT(12); /* Retry test */
pub(crate) const FXMAC_NWCFG_PCSSEL_MASK: u32 = BIT(11); /* PCS Select */
pub(crate) const FXMAC_NWCFG_1000_MASK: u32 = BIT(10); /* Gigabit mode enable */
pub(crate) const FXMAC_NWCFG_XTADDMACHEN_MASK: u32 = BIT(9); /* External address match enable */
pub(crate) const FXMAC_NWCFG_1536RXEN_MASK: u32 = BIT(8); /* Enable 1536 byte frames reception */
pub(crate) const FXMAC_NWCFG_UCASTHASHEN_MASK: u32 = BIT(7); /* Receive unicast hash frames */
pub(crate) const FXMAC_NWCFG_MCASTHASHEN_MASK: u32 = BIT(6); /* Receive multicast hash frames */
pub(crate) const FXMAC_NWCFG_BCASTDI_MASK: u32 = BIT(5); /* Do not receive broadcast frames */
pub(crate) const FXMAC_NWCFG_COPYALLEN_MASK: u32 = BIT(4); /* Copy all frames */
pub(crate) const FXMAC_NWCFG_JUMBO_MASK: u32 = BIT(3); /* Jumbo frames */
pub(crate) const FXMAC_NWCFG_NVLANDISC_MASK: u32 = BIT(2); /* Receive only VLAN frames */
pub(crate) const FXMAC_NWCFG_FDEN_MASK: u32 = BIT(1); /* full duplex */
pub(crate) const FXMAC_NWCFG_100_MASK: u32 = BIT(0); /* 100 Mbps */

/* Receive buffer descriptor status words bit positions.
 * Receive buffer descriptor consists of two 32-bit registers,
 * the first - word0 contains a 32-bit word aligned address pointing to the
 * address of the buffer. The lower two bits make up the wrap bit indicating
 * the last descriptor and the ownership bit to indicate it has been used by
 * the xmac.
 * The following register - word1, contains status information regarding why
 * the frame was received (the filter match condition) as well as other
 * useful info.
 * @{
 */
pub(crate) const FXMAC_RXBUF_BCAST_MASK: u32 = BIT(31); /* Broadcast frame */
pub(crate) const FXMAC_RXBUF_HASH_MASK: u32 = GENMASK(30, 29);
pub(crate) const FXMAC_RXBUF_MULTIHASH_MASK: u32 = BIT(30); /* Multicast hashed frame */
pub(crate) const FXMAC_RXBUF_UNIHASH_MASK: u32 = BIT(29); /* Unicast hashed frame */
pub(crate) const FXMAC_RXBUF_EXH_MASK: u32 = BIT(27); /* buffer exhausted */
pub(crate) const FXMAC_RXBUF_AMATCH_MASK: u32 = GENMASK(26, 25); /* Specific address \
                                                                 matched */
pub(crate) const FXMAC_RXBUF_IDFOUND_MASK: u32 = BIT(24); /* Type ID matched */
pub(crate) const FXMAC_RXBUF_IDMATCH_MASK: u32 = GENMASK(23, 22); /* ID matched mask */
pub(crate) const FXMAC_RXBUF_VLAN_MASK: u32 = BIT(21); /* VLAN tagged */
pub(crate) const FXMAC_RXBUF_PRI_MASK: u32 = BIT(20); /* Priority tagged */
pub(crate) const FXMAC_RXBUF_VPRI_MASK: u32 = GENMASK(19, 17); /* Vlan priority */
pub(crate) const FXMAC_RXBUF_CFI_MASK: u32 = BIT(16); /* CFI frame */
pub(crate) const FXMAC_RXBUF_EOF_MASK: u32 = BIT(15); /* End of frame. */
pub(crate) const FXMAC_RXBUF_SOF_MASK: u32 = BIT(14); /* Start of frame. */
pub(crate) const FXMAC_RXBUF_FCS_STATUS_MASK: u32 = BIT(13); /* Status of fcs. */
pub(crate) const FXMAC_RXBUF_LEN_MASK: u32 = GENMASK(12, 0); /* Mask for length field */
pub(crate) const FXMAC_RXBUF_LEN_JUMBO_MASK: u32 = GENMASK(13, 0); /* Mask for jumbo length */

pub(crate) const FXMAC_RXBUF_WRAP_MASK: u32 = BIT(1); /* Wrap bit, last BD */
pub(crate) const FXMAC_RXBUF_NEW_MASK: u32 = BIT(0); /* Used bit.. */
pub(crate) const FXMAC_RXBUF_ADD_MASK: u32 = GENMASK(31, 2); /* Mask for address */

/*
 * @}
 */

/* Transmit buffer descriptor status words bit positions.
 * Transmit buffer descriptor consists of two 32-bit registers,
 * the first - word0 contains a 32-bit address pointing to the location of
 * the transmit data.
 * The following register - word1, consists of various information to control
 * the xmac transmit process.  After transmit, this is updated with status
 * information, whether the frame was transmitted OK or why it had failed.
 * @{
 */
pub(crate) const FXMAC_TXBUF_USED_MASK: u32 = BIT(31); /* Used bit. */
pub(crate) const FXMAC_TXBUF_WRAP_MASK: u32 = BIT(30); /* Wrap bit, last descriptor */
pub(crate) const FXMAC_TXBUF_RETRY_MASK: u32 = BIT(29); /* Retry limit exceeded */
pub(crate) const FXMAC_TXBUF_URUN_MASK: u32 = BIT(28); /* Transmit underrun occurred */
pub(crate) const FXMAC_TXBUF_EXH_MASK: u32 = BIT(27); /* Buffers exhausted */
pub(crate) const FXMAC_TXBUF_TCP_MASK: u32 = BIT(26); /* Late collision. */
pub(crate) const FXMAC_TXBUF_NOCRC_MASK: u32 = BIT(16); /* No CRC */
pub(crate) const FXMAC_TXBUF_LAST_MASK: u32 = BIT(15); /* Last buffer */
pub(crate) const FXMAC_TXBUF_LEN_MASK: u32 = GENMASK(13, 0); /* Mask for length field */
/*
 * @}
 */

/**
 * @name receive status register bit definitions
 * @{
 */
pub(crate) const FXMAC_RXSR_HRESPNOK_MASK: u32 = BIT(3); /* Receive hresp not OK */
pub(crate) const FXMAC_RXSR_RXOVR_MASK: u32 = BIT(2); /* Receive overrun */
pub(crate) const FXMAC_RXSR_FRAMERX_MASK: u32 = BIT(1); /* Frame received OK */
pub(crate) const FXMAC_RXSR_BUFFNA_MASK: u32 = BIT(0); /* RX buffer used bit set */

pub(crate) const FXMAC_RXSR_ERROR_MASK: u32 =
    (FXMAC_RXSR_HRESPNOK_MASK | FXMAC_RXSR_RXOVR_MASK | FXMAC_RXSR_BUFFNA_MASK);

pub(crate) const FXMAC_SR_ALL_MASK: u32 = GENMASK(31, 0); /* Mask for full register */

/** @name DMA control register bit definitions
 * @{
 */
pub(crate) const FXMAC_DMACR_ADDR_WIDTH_64: u32 = BIT(30); /* 64 bit address bus */
pub(crate) const FXMAC_DMACR_TXEXTEND_MASK: u32 = BIT(29); /* Tx Extended desc mode */
pub(crate) const FXMAC_DMACR_RXEXTEND_MASK: u32 = BIT(28); /* Rx Extended desc mode */
pub(crate) const FXMAC_DMACR_ORCE_DISCARD_ON_ERR_MASK: u32 = BIT(24); /* Auto Discard RX frames during lack of resource. */
pub(crate) const FXMAC_DMACR_RXBUF_MASK: u32 = GENMASK(23, 16); /* Mask bit for RX buffer size */
pub(crate) const FXMAC_DMACR_RXBUF_SHIFT: u32 = 16; /* Shift bit for RX buffer size */
pub(crate) const FXMAC_DMACR_TCPCKSUM_MASK: u32 = BIT(11); /* enable/disable TX checksum offload */
pub(crate) const FXMAC_DMACR_TXSIZE_MASK: u32 = BIT(10); /* TX buffer memory size bit[10] */
pub(crate) const FXMAC_DMACR_RXSIZE_MASK: u32 = GENMASK(9, 8); /* RX buffer memory size bit[9:8] */
pub(crate) const FXMAC_DMACR_ENDIAN_MASK: u32 = BIT(7); /* endian configuration */
pub(crate) const FXMAC_DMACR_SWAP_MANAGEMENT_MASK: u32 = BIT(6); /*  When clear, selects little endian mode */
pub(crate) const FXMAC_DMACR_BLENGTH_MASK: u32 = GENMASK(4, 0); /* buffer burst length */
pub(crate) const FXMAC_DMACR_SINGLE_AHB_AXI_BURST: u32 = BIT(0); /* single AHB_AXI bursts */
pub(crate) const FXMAC_DMACR_INCR4_AHB_AXI_BURST: u32 = BIT(2); /* 4 bytes AHB_AXI bursts */
pub(crate) const FXMAC_DMACR_INCR8_AHB_AXI_BURST: u32 = BIT(3); /* 8 bytes AHB_AXI bursts */
pub(crate) const FXMAC_DMACR_INCR16_AHB_AXI_BURST: u32 = BIT(4); /* 16 bytes AHB_AXI bursts */

/* This register indicates module identification number and module revision. */

pub(crate) const FXMAC_REVISION_MODULE_MASK: u32 = GENMASK(15, 0); /* Module revision */
pub(crate) const FXMAC_IDENTIFICATION_MASK: u32 = GENMASK(27, 16); /* Module identification number */
pub(crate) const FXMAC_FIX_NUM_MASK: u32 = GENMASK(31, 28); /*  Fix number - incremented for fix releases */

/** @name network status register bit definitaions
 * @{
 */
pub(crate) const FXMAC_NWSR_MDIOIDLE_MASK: u32 = BIT(2); /* PHY management idle */
pub(crate) const FXMAC_NWSR_MDIO_MASK: u32 = BIT(1); /* Status of mdio_in */
pub(crate) const FXMAC_NWSR_PCS_LINK_STATE_MASK: u32 = BIT(0);

/** @name PHY Maintenance bit definitions
 * @{
 */
pub(crate) const FXMAC_PHYMNTNC_OP_MASK: u32 = (BIT(17) | BIT(30)); /* operation mask bits */
pub(crate) const FXMAC_PHYMNTNC_OP_R_MASK: u32 = BIT(29); /* read operation */
pub(crate) const FXMAC_PHYMNTNC_OP_W_MASK: u32 = BIT(28); /* write operation */
pub(crate) const FXMAC_PHYMNTNC_ADDR_MASK: u32 = GENMASK(27, 23); /* Address bits */
pub(crate) const FXMAC_PHYMNTNC_REG_MASK: u32 = GENMASK(22, 18); /* register bits */
pub(crate) const FXMAC_PHYMNTNC_DATA_MASK: u32 = GENMASK(11, 0); /* data bits */
pub(crate) const FXMAC_PHYMNTNC_PHAD_SHFT_MSK: u32 = 23; /* Shift bits for PHYAD */
pub(crate) const FXMAC_PHYMNTNC_PREG_SHFT_MSK: u32 = 18; /* Shift bits for PHREG */

/** @name transmit status register bit definitions
 * @{
 */
pub(crate) const FXMAC_TXSR_HRESPNOK_MASK: u32 = BIT(8); /* Transmit hresp not OK */
pub(crate) const FXMAC_TXSR_URUN_MASK: u32 = BIT(6); /* Transmit underrun */
pub(crate) const FXMAC_TXSR_TXCOMPL_MASK: u32 = BIT(5); /* Transmit completed OK */
pub(crate) const FXMAC_TXSR_BUFEXH_MASK: u32 = BIT(4); /* Transmit buffs exhausted mid frame */
pub(crate) const FXMAC_TXSR_TXGO_MASK: u32 = BIT(3); /* Status of go flag */
pub(crate) const FXMAC_TXSR_RXOVR_MASK: u32 = BIT(2); /* Retry limit exceeded */
pub(crate) const FXMAC_TXSR_FRAMERX_MASK: u32 = BIT(1); /* Collision tx frame */
pub(crate) const FXMAC_TXSR_USEDREAD_MASK: u32 = BIT(0); /* TX buffer used bit set */

pub(crate) const FXMAC_TXSR_ERROR_MASK: u32 = (FXMAC_TXSR_HRESPNOK_MASK
    | FXMAC_TXSR_URUN_MASK
    | FXMAC_TXSR_BUFEXH_MASK
    | FXMAC_TXSR_RXOVR_MASK
    | FXMAC_TXSR_FRAMERX_MASK
    | FXMAC_TXSR_USEDREAD_MASK);
/** @name transmit SRAM segment allocation by queue 0 to 7  register bit definitions
 * @{
 */
pub(crate) const FXMAC_TXQSEGALLOC_QLOWER_JUMBO_MASK: u32 = BIT(2); /* 16 segments are distributed to queue 0*/
/**
 * @name Interrupt Q1 status register bit definitions
 * @{
 */
pub(crate) const FXMAC_INTQ1SR_TXCOMPL_MASK: u32 = BIT(7); /* Transmit completed OK */
pub(crate) const FXMAC_INTQ1SR_TXERR_MASK: u32 = BIT(6); /* Transmit AMBA Error */

pub(crate) const FXMAC_INTQ1_IXR_ALL_MASK: u32 =
    (FXMAC_INTQ1SR_TXCOMPL_MASK | FXMAC_INTQ1SR_TXERR_MASK);

/**
 * @name Interrupt QUEUE status register bit definitions
 * @{
 */
pub(crate) const FXMAC_INTQUESR_TXCOMPL_MASK: u32 = BIT(7); /* Transmit completed OK */
pub(crate) const FXMAC_INTQUESR_TXERR_MASK: u32 = BIT(6); /* Transmit AMBA Error */
pub(crate) const FXMAC_INTQUESR_RCOMP_MASK: u32 = BIT(1);
pub(crate) const FXMAC_INTQUESR_RXUBR_MASK: u32 = BIT(2);

pub(crate) const FXMAC_INTQUE_IXR_ALL_MASK: u32 =
    (FXMAC_INTQUESR_TXCOMPL_MASK | FXMAC_INTQUESR_TXERR_MASK);

pub const fn FXMAC_QUEUE_REGISTER_OFFSET(base_addr: u64, queue_id: u32) -> u64 {
    base_addr + (queue_id as u64 - 1) * 4
}

/* Design Configuration Register 1 - The GEM has many parameterisation options to configure the IP during compilation stage. */

pub(crate) const FXMAC_DESIGNCFG_DEBUG1_BUS_WIDTH_MASK: u32 = GENMASK(27, 25);
pub(crate) const FXMAC_DESIGNCFG_DEBUG1_BUS_IRQCOR_MASK: u32 = BIT(23);

/*GEM hs mac config register bitfields*/
pub(crate) const FXMAC_GEM_HSMACSPEED_OFFSET: u64 = 0;
pub(crate) const FXMAC_GEM_HSMACSPEED_SIZE: u32 = 3;
pub(crate) const FXMAC_GEM_HSMACSPEED_MASK: u32 = 0x7;

/* Transmit buffer descriptor status words offset
 * @{
 */
pub(crate) const FXMAC_BD_ADDR_OFFSET: u64 = 0x00000000; /* word 0/addr of BDs */
pub(crate) const FXMAC_BD_STAT_OFFSET: u64 = 4; /* word 1/status of BDs, 4 bytes */
pub(crate) const FXMAC_BD_ADDR_HI_OFFSET: u32 = BIT(3); /* word 2/addr of BDs */

/** @name MAC address register word 1 mask
 * @{
 */
pub(crate) const FXMAC_GEM_SAB_MASK: u32 = GENMASK(15, 0); /* Address bits[47:32] bit[31:0] are in BOTTOM */

/* USXGMII control register FXMAC_GEM_USX_CONTROL_OFFSET */
pub(crate) const FXMAC_GEM_USX_HS_MAC_SPEED_100M: u32 = (0x0 << 14); /* 100M operation */
pub(crate) const FXMAC_GEM_USX_HS_MAC_SPEED_1G: u32 = (0x1 << 14); /* 1G operation */
pub(crate) const FXMAC_GEM_USX_HS_MAC_SPEED_2_5G: u32 = (0x2 << 14); /* 2.5G operation */
pub(crate) const FXMAC_GEM_USX_HS_MAC_SPEED_5G: u32 = (0x3 << 14); /* 5G operation */
pub(crate) const FXMAC_GEM_USX_HS_MAC_SPEED_10G: u32 = (0x4 << 14); /* 10G operation */
pub(crate) const FXMAC_GEM_USX_SERDES_RATE_5G: u32 = (0x0 << 12);
pub(crate) const FXMAC_GEM_USX_SERDES_RATE_10G: u32 = (0x1 << 12);
pub(crate) const FXMAC_GEM_USX_TX_SCR_BYPASS: u32 = BIT(8); /* RX Scrambler Bypass. Set high to bypass the receive descrambler. */
pub(crate) const FXMAC_GEM_USX_RX_SCR_BYPASS: u32 = BIT(9); /* TX Scrambler Bypass. Set high to bypass the transmit scrambler. */
pub(crate) const FXMAC_GEM_USX_RX_SYNC_RESET: u32 = BIT(2); /* RX Reset. Set high to reset the receive datapath. When low the receive datapath is enabled. */
pub(crate) const FXMAC_GEM_USX_TX_DATAPATH_EN: u32 = BIT(1); /* TX Datapath Enable. */
pub(crate) const FXMAC_GEM_USX_SIGNAL_OK: u32 = BIT(0); /* Enable the USXGMII/BASE-R receive PCS. */

/*  All PCS registers */
pub(crate) const FXMAC_PCS_CONTROL_ENABLE_AUTO_NEG: u32 = BIT(12); /* Enable auto-negotiation - when set active high, autonegotiation operation is enabled.  */

/* FXMAC_PCS_STATUS_OFFSET */
pub(crate) const FXMAC_PCS_STATUS_LINK_STATUS_OFFSET: u32 = 2;
pub(crate) const FXMAC_PCS_STATUS_LINK_STATUS: u32 = BIT(FXMAC_PCS_STATUS_LINK_STATUS_OFFSET); /* Link status - indicates the status of the physical connection to the link partner. When set to logic 1 the link is up, and when set to logic 0, the link is down. */

/* FXMAC_PCS_AN_LP_OFFSET */
pub(crate) const FXMAC_PCS_AN_LP_SPEED_OFFSET: u64 = 10;
pub(crate) const FXMAC_PCS_AN_LP_SPEED: u32 = (0x3 << FXMAC_PCS_AN_LP_SPEED_OFFSET); /* SGMII 11 : Reserved 10 : 1000 Mbps 01 : 100Mbps 00 : 10 Mbps */
pub(crate) const FXMAC_PCS_AN_LP_DUPLEX_OFFSET: u64 = 12;
pub(crate) const FXMAC_PCS_AN_LP_DUPLEX: u32 = (0x3 << FXMAC_PCS_AN_LP_DUPLEX_OFFSET); /* SGMII Bit 13: Reserved. read as 0. Bit 12 : 0 : half-duplex. 1: Full Duplex." */
pub(crate) const FXMAC_PCS_LINK_PARTNER_NEXT_PAGE_STATUS: u32 = (1 << 15); /* In sgmii mode, 0 is link down . 1 is link up */
pub(crate) const FXMAC_PCS_LINK_PARTNER_NEXT_PAGE_OFFSET: u64 = 15;

/* USXGMII Status Register */
pub(crate) const FXMAC_GEM_USX_STATUS_BLOCK_LOCK: u32 = BIT(0); /* Block Lock. A value of one indicates that the PCS has achieved block synchronization. */

// aarch64
pub(crate) const BITS_PER_LONG: u32 = 64;
pub const fn BIT(n: u32) -> u32 {
    1 << n
}

pub const fn GENMASK(h: u32, l: u32) -> u32 {
    ((!0_u64 - (1 << l) + 1) & (!0_u64 >> (BITS_PER_LONG - 1 - h))) as u32
}

////////////////////
// fxmac.h

pub(crate) const FXMAC_PROMISC_OPTION: u32 = 0x00000001;
/* Accept all incoming packets.
 *   This option defaults to disabled (cleared) */

pub(crate) const FXMAC_FRAME1536_OPTION: u32 = 0x00000002;
/* Frame larger than 1516 support for Tx & Rx.x
 *   This option defaults to disabled (cleared) */

pub(crate) const FXMAC_VLAN_OPTION: u32 = 0x00000004;
/* VLAN Rx & Tx frame support.
 *   This option defaults to disabled (cleared) */

pub(crate) const FXMAC_FLOW_CONTROL_OPTION: u32 = 0x00000010;
/* Enable recognition of flow control frames on Rx
 *   This option defaults to enabled (set) */

pub(crate) const FXMAC_FCS_STRIP_OPTION: u32 = 0x00000020;
/* Strip FCS and PAD from incoming frames. Note: PAD from VLAN frames is not
 *   stripped.
 *   This option defaults to enabled (set) */

pub(crate) const FXMAC_FCS_INSERT_OPTION: u32 = 0x00000040;
/* Generate FCS field and add PAD automatically for outgoing frames.
 *   This option defaults to disabled (cleared) */

pub(crate) const FXMAC_LENTYPE_ERR_OPTION: u32 = 0x00000080;
/* Enable Length/Type error checking for incoming frames. When this option is
 *   set, the MAC will filter frames that have a mismatched type/length field
 *   and if FXMAC_REPORT_RXERR_OPTION is set, the user is notified when these
 *   types of frames are encountered. When this option is cleared, the MAC will
 *   allow these types of frames to be received.
 *
 *   This option defaults to disabled (cleared) */

pub(crate) const FXMAC_TRANSMITTER_ENABLE_OPTION: u32 = 0x00000100;
/* Enable the transmitter.
 *   This option defaults to enabled (set) */

pub(crate) const FXMAC_RECEIVER_ENABLE_OPTION: u32 = 0x00000200;
/* Enable the receiver
 *   This option defaults to enabled (set) */

pub(crate) const FXMAC_BROADCAST_OPTION: u32 = 0x00000400;
/* Allow reception of the broadcast address
 *   This option defaults to enabled (set) */

pub(crate) const FXMAC_MULTICAST_OPTION: u32 = 0x00000800;
/* Allows reception of multicast addresses programmed into hash
 *   This option defaults to disabled (clear) */

pub(crate) const FXMAC_RX_CHKSUM_ENABLE_OPTION: u32 = 0x00001000;
/* Enable the RX checksum offload
 *   This option defaults to enabled (set) */

pub(crate) const FXMAC_TX_CHKSUM_ENABLE_OPTION: u32 = 0x00002000;
/* Enable the TX checksum offload
 *   This option defaults to enabled (set) */

pub(crate) const FXMAC_JUMBO_ENABLE_OPTION: u32 = 0x00004000;
pub(crate) const FXMAC_SGMII_ENABLE_OPTION: u32 = 0x00008000;

pub(crate) const FXMAC_LOOPBACK_NO_MII_OPTION: u32 = 0x00010000;
pub(crate) const FXMAC_LOOPBACK_USXGMII_OPTION: u32 = 0x00020000;

pub(crate) const FXMAC_UNICAST_OPTION: u32 = 0x00040000;

pub(crate) const FXMAC_TAIL_PTR_OPTION: u32 = 0x00080000;

pub(crate) const FXMAC_DEFAULT_OPTIONS: u32 = (FXMAC_FLOW_CONTROL_OPTION
    | FXMAC_FCS_INSERT_OPTION
    | FXMAC_FCS_STRIP_OPTION
    | FXMAC_BROADCAST_OPTION
    | FXMAC_LENTYPE_ERR_OPTION
    | FXMAC_TRANSMITTER_ENABLE_OPTION
    | FXMAC_RECEIVER_ENABLE_OPTION
    | FXMAC_RX_CHKSUM_ENABLE_OPTION
    | FXMAC_TX_CHKSUM_ENABLE_OPTION);

/* The next few constants help upper layers determine the size of memory
 * pools used for Ethernet buffers and descriptor lists.
 */
pub(crate) const FXMAC_MAC_ADDR_SIZE: u32 = 6; /* size of Ethernet header */

pub(crate) const FXMAC_MTU: u32 = 1500; /* max MTU size of Ethernet frame */
pub(crate) const FXMAC_MTU_JUMBO: u32 = 10240; /* max MTU size of jumbo frame including Ip header + IP payload */
pub(crate) const FXMAC_HDR_SIZE: u32 = 14; /* size of Ethernet header  , DA + SA + TYPE*/
pub(crate) const FXMAC_HDR_VLAN_SIZE: u32 = 18; /* size of Ethernet header with VLAN */
pub(crate) const FXMAC_TRL_SIZE: u32 = 4; /* size of Ethernet trailer (FCS) */

pub(crate) const FXMAC_MAX_FRAME_SIZE: u32 = (FXMAC_MTU + FXMAC_HDR_SIZE + FXMAC_TRL_SIZE);
pub(crate) const FXMAC_MAX_FRAME_SIZE_JUMBO: u32 =
    (FXMAC_MTU_JUMBO + FXMAC_HDR_SIZE + FXMAC_TRL_SIZE);

pub(crate) const FXMAC_MAX_VLAN_FRAME_SIZE: u32 =
    (FXMAC_MTU + FXMAC_HDR_SIZE + FXMAC_HDR_VLAN_SIZE + FXMAC_TRL_SIZE);
pub(crate) const FXMAC_MAX_VLAN_FRAME_SIZE_JUMBO: u32 =
    (FXMAC_MTU_JUMBO + FXMAC_HDR_SIZE + FXMAC_HDR_VLAN_SIZE + FXMAC_TRL_SIZE);

/** @name Callback identifiers
 *
 * These constants are used as parameters to FXMAC_SetHandler()
 * @{
 */
pub(crate) const FXMAC_HANDLER_DMASEND: u32 = 1; /* 发送中断 */
pub(crate) const FXMAC_HANDLER_DMARECV: u32 = 2; /* 接收中断 */
pub(crate) const FXMAC_HANDLER_ERROR: u32 = 3; /* 异常中断 */
pub(crate) const FXMAC_HANDLER_LINKCHANGE: u32 = 4; /* 连接状态 */
pub(crate) const FXMAC_HANDLER_RESTART: u32 = 5; /* 发送描述符队列发生异常 */
/*@}*/

pub(crate) const FXMAC_DMA_SG_IS_STARTED: u32 = 0;
pub(crate) const FXMAC_DMA_SG_IS_STOPED: u32 = 1;

pub(crate) const FXMAC_SPEED_10: u32 = 10;
pub(crate) const FXMAC_SPEED_100: u32 = 100;
pub(crate) const FXMAC_SPEED_1000: u32 = 1000;
pub(crate) const FXMAC_SPEED_2500: u32 = 2500;
pub(crate) const FXMAC_SPEED_5000: u32 = 5000;
pub(crate) const FXMAC_SPEED_10000: u32 = 10000;
pub(crate) const FXMAC_SPEED_25000: u32 = 25000;

/*  Capability mask bits */
pub(crate) const FXMAC_CAPS_ISR_CLEAR_ON_WRITE: u32 = 0x00000001; /* irq status parameters need to be written to clear after they have been read */
pub(crate) const FXMAC_CAPS_TAILPTR: u32 = 0x00000002; /* use tail ptr */

/*
Direction identifiers
These are used by several functions and callbacks that need
to specify whether an operation specifies a send or receive channel.

pub(crate) const FXMAC_PHY_INTERFACE_MODE_2500BASEX: u32 = 6;
pub(crate) const FXMAC_PHY_INTERFACE_MODE_5GBASER: u32 = 5;
pub(crate) const FXMAC_PHY_INTERFACE_MODE_USXGMII: u32 = 4;
pub(crate) const FXMAC_PHY_INTERFACE_MODE_XGMII: u32 = 3;
pub(crate) const FXMAC_PHY_INTERFACE_MODE_RGMII: u32 = 2;
pub(crate) const FXMAC_PHY_INTERFACE_MODE_RMII: u32 = 1;
pub(crate) const FXMAC_PHY_INTERFACE_MODE_SGMII: u32 = 0;
*/

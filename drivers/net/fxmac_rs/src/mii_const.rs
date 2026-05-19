// linux/mii.h: definitions for MII-compatible transceivers

/* Generic MII registers. */
pub(crate) const MII_BMCR: u32 = 0x00; /* Basic mode control register */
pub(crate) const MII_BMSR: u32 = 0x01; /* Basic mode status register  */
pub(crate) const MII_PHYSID1: u32 = 0x02; /* PHYS ID 1                   */
pub(crate) const MII_PHYSID2: u32 = 0x03; /* PHYS ID 2                   */
pub(crate) const MII_ADVERTISE: u32 = 0x04; /* Advertisement control reg   */
pub(crate) const MII_LPA: u32 = 0x05; /* Link partner ability reg    */
pub(crate) const MII_EXPANSION: u32 = 0x06; /* Expansion register          */
pub(crate) const MII_CTRL1000: u32 = 0x09; /* 1000BASE-T control          */
pub(crate) const MII_STAT1000: u32 = 0x0a; /* 1000BASE-T status           */
pub(crate) const MII_MMD_CTRL: u32 = 0x0d; /* MMD Access Control Register */
pub(crate) const MII_MMD_DATA: u32 = 0x0e; /* MMD Access Data Register */
pub(crate) const MII_ESTATUS: u32 = 0x0f; /* Extended Status             */
pub(crate) const MII_DCOUNTER: u32 = 0x12; /* Disconnect counter          */
pub(crate) const MII_FCSCOUNTER: u32 = 0x13; /* False carrier counter       */
pub(crate) const MII_NWAYTEST: u32 = 0x14; /* N-way auto-neg test reg     */
pub(crate) const MII_RERRCOUNTER: u32 = 0x15; /* Receive error counter       */
pub(crate) const MII_SREVISION: u32 = 0x16; /* Silicon revision            */
pub(crate) const MII_RESV1: u32 = 0x17; /* Reserved...                 */
pub(crate) const MII_LBRERROR: u32 = 0x18; /* Lpback, rx, bypass error    */
pub(crate) const MII_PHYADDR: u32 = 0x19; /* PHY address                 */
pub(crate) const MII_RESV2: u32 = 0x1a; /* Reserved...                 */
pub(crate) const MII_TPISTATUS: u32 = 0x1b; /* TPI status for 10mbps       */
pub(crate) const MII_NCONFIG: u32 = 0x1c; /* Network interface config    */

/* Basic mode control register. */
pub(crate) const BMCR_RESV: u32 = 0x003f; /* Unused...                   */
pub(crate) const BMCR_SPEED1000: u32 = 0x0040; /* MSB of Speed (1000)         */
pub(crate) const BMCR_CTST: u32 = 0x0080; /* Collision test              */
pub(crate) const BMCR_FULLDPLX: u32 = 0x0100; /* Full duplex                 */
pub(crate) const BMCR_ANRESTART: u32 = 0x0200; /* Auto negotiation restart    */
pub(crate) const BMCR_ISOLATE: u32 = 0x0400; /* Isolate data paths from MII */
pub(crate) const BMCR_PDOWN: u32 = 0x0800; /* Enable low power state      */
pub(crate) const BMCR_ANENABLE: u32 = 0x1000; /* Enable auto negotiation     */
pub(crate) const BMCR_SPEED100: u32 = 0x2000; /* Select 100Mbps              */
pub(crate) const BMCR_LOOPBACK: u32 = 0x4000; /* TXD loopback bits           */
pub(crate) const BMCR_RESET: u32 = 0x8000; /* Reset to default state      */
pub(crate) const BMCR_SPEED10: u32 = 0x0000; /* Select 10Mbps               */

/* Basic mode status register. */
pub(crate) const BMSR_ERCAP: u32 = 0x0001; /* Ext-reg capability          */
pub(crate) const BMSR_JCD: u32 = 0x0002; /* Jabber detected             */
pub(crate) const BMSR_LSTATUS: u32 = 0x0004; /* Link status                 */
pub(crate) const BMSR_ANEGCAPABLE: u32 = 0x0008; /* Able to do auto-negotiation */
pub(crate) const BMSR_RFAULT: u32 = 0x0010; /* Remote fault detected       */
pub(crate) const BMSR_ANEGCOMPLETE: u32 = 0x0020; /* Auto-negotiation complete   */
pub(crate) const BMSR_RESV: u32 = 0x00c0; /* Unused...                   */
pub(crate) const BMSR_ESTATEN: u32 = 0x0100; /* Extended Status in R15      */
pub(crate) const BMSR_100HALF2: u32 = 0x0200; /* Can do 100BASE-T2 HDX       */
pub(crate) const BMSR_100FULL2: u32 = 0x0400; /* Can do 100BASE-T2 FDX       */
pub(crate) const BMSR_10HALF: u32 = 0x0800; /* Can do 10mbps, half-duplex  */
pub(crate) const BMSR_10FULL: u32 = 0x1000; /* Can do 10mbps, full-duplex  */
pub(crate) const BMSR_100HALF: u32 = 0x2000; /* Can do 100mbps, half-duplex */
pub(crate) const BMSR_100FULL: u32 = 0x4000; /* Can do 100mbps, full-duplex */
pub(crate) const BMSR_100BASE4: u32 = 0x8000; /* Can do 100mbps, 4k packets  */

/* Advertisement control register. */
pub(crate) const ADVERTISE_SLCT: u32 = 0x001f; /* Selector bits               */
pub(crate) const ADVERTISE_CSMA: u32 = 0x0001; /* Only selector supported     */
pub(crate) const ADVERTISE_10HALF: u32 = 0x0020; /* Try for 10mbps half-duplex  */
pub(crate) const ADVERTISE_1000XFULL: u32 = 0x0020; /* Try for 1000BASE-X full-duplex */
pub(crate) const ADVERTISE_10FULL: u32 = 0x0040; /* Try for 10mbps full-duplex  */
pub(crate) const ADVERTISE_1000XHALF: u32 = 0x0040; /* Try for 1000BASE-X half-duplex */
pub(crate) const ADVERTISE_100HALF: u32 = 0x0080; /* Try for 100mbps half-duplex */
pub(crate) const ADVERTISE_1000XPAUSE: u32 = 0x0080; /* Try for 1000BASE-X pause    */
pub(crate) const ADVERTISE_100FULL: u32 = 0x0100; /* Try for 100mbps full-duplex */
pub(crate) const ADVERTISE_1000XPSE_ASYM: u32 = 0x0100; /* Try for 1000BASE-X asym pause */
pub(crate) const ADVERTISE_100BASE4: u32 = 0x0200; /* Try for 100mbps 4k packets  */
pub(crate) const ADVERTISE_PAUSE_CAP: u32 = 0x0400; /* Try for pause               */
pub(crate) const ADVERTISE_PAUSE_ASYM: u32 = 0x0800; /* Try for asymetric pause     */
pub(crate) const ADVERTISE_RESV: u32 = 0x1000; /* Unused...                   */
pub(crate) const ADVERTISE_RFAULT: u32 = 0x2000; /* Say we can detect faults    */
pub(crate) const ADVERTISE_LPACK: u32 = 0x4000; /* Ack link partners response  */
pub(crate) const ADVERTISE_NPAGE: u32 = 0x8000; /* Next page bit               */

pub(crate) const ADVERTISE_FULL: u32 = ADVERTISE_100FULL | ADVERTISE_10FULL | ADVERTISE_CSMA;
pub(crate) const ADVERTISE_ALL: u32 =
    ADVERTISE_10HALF | ADVERTISE_10FULL | ADVERTISE_100HALF | ADVERTISE_100FULL;

/* Link partner ability register. */
pub(crate) const LPA_SLCT: u32 = 0x001f; /* Same as advertise selector  */
pub(crate) const LPA_10HALF: u32 = 0x0020; /* Can do 10mbps half-duplex   */
pub(crate) const LPA_1000XFULL: u32 = 0x0020; /* Can do 1000BASE-X full-duplex */
pub(crate) const LPA_10FULL: u32 = 0x0040; /* Can do 10mbps full-duplex   */
pub(crate) const LPA_1000XHALF: u32 = 0x0040; /* Can do 1000BASE-X half-duplex */
pub(crate) const LPA_100HALF: u32 = 0x0080; /* Can do 100mbps half-duplex  */
pub(crate) const LPA_1000XPAUSE: u32 = 0x0080; /* Can do 1000BASE-X pause     */
pub(crate) const LPA_100FULL: u32 = 0x0100; /* Can do 100mbps full-duplex  */
pub(crate) const LPA_1000XPAUSE_ASYM: u32 = 0x0100; /* Can do 1000BASE-X pause asym*/
pub(crate) const LPA_100BASE4: u32 = 0x0200; /* Can do 100mbps 4k packets   */
pub(crate) const LPA_PAUSE_CAP: u32 = 0x0400; /* Can pause                   */
pub(crate) const LPA_PAUSE_ASYM: u32 = 0x0800; /* Can pause asymetrically     */
pub(crate) const LPA_RESV: u32 = 0x1000; /* Unused...                   */
pub(crate) const LPA_RFAULT: u32 = 0x2000; /* Link partner faulted        */
pub(crate) const LPA_LPACK: u32 = 0x4000; /* Link partner acked us       */
pub(crate) const LPA_NPAGE: u32 = 0x8000; /* Next page bit               */

pub(crate) const LPA_DUPLEX: u32 = LPA_10FULL | LPA_100FULL;
pub(crate) const LPA_100: u32 = LPA_100FULL | LPA_100HALF | LPA_100BASE4;

/* Expansion register for auto-negotiation. */
pub(crate) const EXPANSION_NWAY: u32 = 0x0001; /* Can do N-way auto-nego      */
pub(crate) const EXPANSION_LCWP: u32 = 0x0002; /* Got new RX page code word   */
pub(crate) const EXPANSION_ENABLENPAGE: u32 = 0x0004; /* This enables npage words    */
pub(crate) const EXPANSION_NPCAPABLE: u32 = 0x0008; /* Link partner supports npage */
pub(crate) const EXPANSION_MFAULTS: u32 = 0x0010; /* Multiple faults detected    */
pub(crate) const EXPANSION_RESV: u32 = 0xffe0; /* Unused...                   */

pub(crate) const ESTATUS_1000_XFULL: u32 = 0x8000; /* Can do 1000BaseX Full       */
pub(crate) const ESTATUS_1000_XHALF: u32 = 0x4000; /* Can do 1000BaseX Half       */
pub(crate) const ESTATUS_1000_TFULL: u32 = 0x2000; /* Can do 1000BT Full          */
pub(crate) const ESTATUS_1000_THALF: u32 = 0x1000; /* Can do 1000BT Half          */

/* N-way test register. */
pub(crate) const NWAYTEST_RESV1: u32 = 0x00ff; /* Unused...                   */
pub(crate) const NWAYTEST_LOOPBACK: u32 = 0x0100; /* Enable loopback for N-way   */
pub(crate) const NWAYTEST_RESV2: u32 = 0xfe00; /* Unused...                   */

/* MAC and PHY tx_config_Reg[15:0] for SGMII in-band auto-negotiation.*/
pub(crate) const ADVERTISE_SGMII: u32 = 0x0001; /* MAC can do SGMII            */
pub(crate) const LPA_SGMII: u32 = 0x0001; /* PHY can do SGMII            */
pub(crate) const LPA_SGMII_SPD_MASK: u32 = 0x0c00; /* SGMII speed mask            */
pub(crate) const LPA_SGMII_FULL_DUPLEX: u32 = 0x1000; /* SGMII full duplex           */
pub(crate) const LPA_SGMII_DPX_SPD_MASK: u32 = 0x1C00; /* SGMII duplex and speed bits */
pub(crate) const LPA_SGMII_10: u32 = 0x0000; /* 10Mbps                      */
pub(crate) const LPA_SGMII_10HALF: u32 = 0x0000; /* Can do 10mbps half-duplex   */
pub(crate) const LPA_SGMII_10FULL: u32 = 0x1000; /* Can do 10mbps full-duplex   */
pub(crate) const LPA_SGMII_100: u32 = 0x0400; /* 100Mbps                     */
pub(crate) const LPA_SGMII_100HALF: u32 = 0x0400; /* Can do 100mbps half-duplex  */
pub(crate) const LPA_SGMII_100FULL: u32 = 0x1400; /* Can do 100mbps full-duplex  */
pub(crate) const LPA_SGMII_1000: u32 = 0x0800; /* 1000Mbps                    */
pub(crate) const LPA_SGMII_1000HALF: u32 = 0x0800; /* Can do 1000mbps half-duplex */
pub(crate) const LPA_SGMII_1000FULL: u32 = 0x1800; /* Can do 1000mbps full-duplex */
pub(crate) const LPA_SGMII_LINK: u32 = 0x8000; /* PHY link with copper-side partner */

/* 1000BASE-T Control register */
pub(crate) const ADVERTISE_1000FULL: u32 = 0x0200; /* Advertise 1000BASE-T full duplex */
pub(crate) const ADVERTISE_1000HALF: u32 = 0x0100; /* Advertise 1000BASE-T half duplex */
pub(crate) const CTL1000_PREFER_MASTER: u32 = 0x0400; /* prefer to operate as master */
pub(crate) const CTL1000_AS_MASTER: u32 = 0x0800;
pub(crate) const CTL1000_ENABLE_MASTER: u32 = 0x1000;

/* 1000BASE-T Status register */
pub(crate) const LPA_1000MSFAIL: u32 = 0x8000; /* Master/Slave resolution failure */
pub(crate) const LPA_1000MSRES: u32 = 0x4000; /* Master/Slave resolution status */
pub(crate) const LPA_1000LOCALRXOK: u32 = 0x2000; /* Link partner local receiver status */
pub(crate) const LPA_1000REMRXOK: u32 = 0x1000; /* Link partner remote receiver status */
pub(crate) const LPA_1000FULL: u32 = 0x0800; /* Link partner 1000BASE-T full duplex */
pub(crate) const LPA_1000HALF: u32 = 0x0400; /* Link partner 1000BASE-T half duplex */

/* Flow control flags */
pub(crate) const FLOW_CTRL_TX: u32 = 0x01;
pub(crate) const FLOW_CTRL_RX: u32 = 0x02;

/* MMD Access Control register fields */
pub(crate) const MII_MMD_CTRL_DEVAD_MASK: u32 = 0x1f; /* Mask MMD DEVAD*/
pub(crate) const MII_MMD_CTRL_ADDR: u32 = 0x0000; /* Address */
pub(crate) const MII_MMD_CTRL_NOINCR: u32 = 0x4000; /* no post increment */
pub(crate) const MII_MMD_CTRL_INCR_RDWT: u32 = 0x8000; /* post increment on reads & writes */
pub(crate) const MII_MMD_CTRL_INCR_ON_WT: u32 = 0xC000; /* post increment on writes only */

// MDIO
pub(crate) const MDIO_PRTAD_NONE: i32 = -1;
pub(crate) const MDIO_DEVAD_NONE: i32 = -1;
pub(crate) const MDIO_EMULATE_C22: i32 = 4;

// PHY
pub(crate) const PHY_FIXED_ID: u32 = 0xa5a55a5a;
pub(crate) const PHY_NCSI_ID: u32 = 0xbeefcafe;
pub(crate) const PHY_GMII2RGMII_ID: u32 = 0x5a5a5a5a;
pub(crate) const PHY_MAX_ADDR: u32 = 32;
pub(crate) const PHY_FLAG_BROKEN_RESET: u32 = 1 << 0; /* soft reset not supported */

/* phy seed setup */
pub(crate) const AUTO: u32 = 99;
pub(crate) const _1000BASET: u32 = 1000;
pub(crate) const _100BASET: u32 = 100;
pub(crate) const _10BASET: u32 = 10;
pub(crate) const HALF: u32 = 22;
pub(crate) const FULL: u32 = 44;

/* phy register offsets */
pub(crate) const MII_MIPSCR: u32 = 0x11;

/* MII_LPA */
pub(crate) const PHY_ANLPAR_PSB_802_3: u32 = 0x0001;
pub(crate) const PHY_ANLPAR_PSB_802_9: u32 = 0x0002;

/* MII_CTRL1000 masks */
pub(crate) const PHY_1000BTCR_1000FD: u32 = 0x0200;
pub(crate) const PHY_1000BTCR_1000HD: u32 = 0x0100;

/* MII_STAT1000 masks */
pub(crate) const PHY_1000BTSR_MSCF: u32 = 0x8000;
pub(crate) const PHY_1000BTSR_MSCR: u32 = 0x4000;
pub(crate) const PHY_1000BTSR_LRS: u32 = 0x2000;
pub(crate) const PHY_1000BTSR_RRS: u32 = 0x1000;
pub(crate) const PHY_1000BTSR_1000FD: u32 = 0x0800;
pub(crate) const PHY_1000BTSR_1000HD: u32 = 0x0400;

/* phy EXSR */
pub(crate) const ESTATUS_1000XF: u32 = 0x8000;
pub(crate) const ESTATUS_1000XH: u32 = 0x4000;

/* Indicates what features are supported by the interface. */
pub(crate) const SUPPORTED_10baseT_Half: u32 = 1 << 0;
pub(crate) const SUPPORTED_10baseT_Full: u32 = 1 << 1;
pub(crate) const SUPPORTED_100baseT_Half: u32 = 1 << 2;
pub(crate) const SUPPORTED_100baseT_Full: u32 = 1 << 3;
pub(crate) const SUPPORTED_1000baseT_Half: u32 = 1 << 4;
pub(crate) const SUPPORTED_1000baseT_Full: u32 = 1 << 5;
pub(crate) const SUPPORTED_Autoneg: u32 = 1 << 6;
pub(crate) const SUPPORTED_TP: u32 = 1 << 7;
pub(crate) const SUPPORTED_AUI: u32 = 1 << 8;
pub(crate) const SUPPORTED_MII: u32 = 1 << 9;
pub(crate) const SUPPORTED_FIBRE: u32 = 1 << 10;
pub(crate) const SUPPORTED_BNC: u32 = 1 << 11;
pub(crate) const SUPPORTED_10000baseT_Full: u32 = 1 << 12;
pub(crate) const SUPPORTED_Pause: u32 = 1 << 13;
pub(crate) const SUPPORTED_Asym_Pause: u32 = 1 << 14;
pub(crate) const SUPPORTED_2500baseX_Full: u32 = 1 << 15;
pub(crate) const SUPPORTED_Backplane: u32 = 1 << 16;
pub(crate) const SUPPORTED_1000baseKX_Full: u32 = 1 << 17;
pub(crate) const SUPPORTED_10000baseKX4_Full: u32 = 1 << 18;
pub(crate) const SUPPORTED_10000baseKR_Full: u32 = 1 << 19;
pub(crate) const SUPPORTED_10000baseR_FEC: u32 = 1 << 20;
pub(crate) const SUPPORTED_1000baseX_Half: u32 = 1 << 21;
pub(crate) const SUPPORTED_1000baseX_Full: u32 = 1 << 22;

/* Indicates what features are advertised by the interface. */
pub(crate) const ADVERTISED_10baseT_Half: u32 = 1 << 0;
pub(crate) const ADVERTISED_10baseT_Full: u32 = 1 << 1;
pub(crate) const ADVERTISED_100baseT_Half: u32 = 1 << 2;
pub(crate) const ADVERTISED_100baseT_Full: u32 = 1 << 3;
pub(crate) const ADVERTISED_1000baseT_Half: u32 = 1 << 4;
pub(crate) const ADVERTISED_1000baseT_Full: u32 = 1 << 5;
pub(crate) const ADVERTISED_Autoneg: u32 = 1 << 6;
pub(crate) const ADVERTISED_TP: u32 = 1 << 7;
pub(crate) const ADVERTISED_AUI: u32 = 1 << 8;
pub(crate) const ADVERTISED_MII: u32 = 1 << 9;
pub(crate) const ADVERTISED_FIBRE: u32 = 1 << 10;
pub(crate) const ADVERTISED_BNC: u32 = 1 << 11;
pub(crate) const ADVERTISED_10000baseT_Full: u32 = 1 << 12;
pub(crate) const ADVERTISED_Pause: u32 = 1 << 13;
pub(crate) const ADVERTISED_Asym_Pause: u32 = 1 << 14;
pub(crate) const ADVERTISED_2500baseX_Full: u32 = 1 << 15;
pub(crate) const ADVERTISED_Backplane: u32 = 1 << 16;
pub(crate) const ADVERTISED_1000baseKX_Full: u32 = 1 << 17;
pub(crate) const ADVERTISED_10000baseKX4_Full: u32 = 1 << 18;
pub(crate) const ADVERTISED_10000baseKR_Full: u32 = 1 << 19;
pub(crate) const ADVERTISED_10000baseR_FEC: u32 = 1 << 20;
pub(crate) const ADVERTISED_1000baseX_Half: u32 = 1 << 21;
pub(crate) const ADVERTISED_1000baseX_Full: u32 = 1 << 22;

/* The forced speed, 10Mb, 100Mb, gigabit, 2.5Gb, 10GbE. */
pub(crate) const SPEED_10: u32 = 10;
pub(crate) const SPEED_100: u32 = 100;
pub(crate) const SPEED_1000: u32 = 1000;
pub(crate) const SPEED_2500: u32 = 2500;
pub(crate) const SPEED_10000: u32 = 10000;
pub(crate) const SPEED_14000: u32 = 14000;
pub(crate) const SPEED_20000: u32 = 20000;
pub(crate) const SPEED_25000: u32 = 25000;
pub(crate) const SPEED_40000: u32 = 40000;
pub(crate) const SPEED_50000: u32 = 50000;
pub(crate) const SPEED_56000: u32 = 56000;
pub(crate) const SPEED_100000: u32 = 100000;
pub(crate) const SPEED_200000: u32 = 200000;

/* Duplex, half or full. */
pub(crate) const DUPLEX_HALF: u32 = 0x00;
pub(crate) const DUPLEX_FULL: u32 = 0x01;

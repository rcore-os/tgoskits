use bitfield_struct::bitfield;
use volatile::{
    VolatileFieldAccess,
    access::{ReadOnly, WriteOnly},
};

#[repr(C)]
#[derive(VolatileFieldAccess)]
pub struct RegisterBlock {
    /// Control Register
    pub ctrl: Ctrl,

    /// Power Enable Register
    pub pwren: PwrEn,

    /// Clock Divider Register
    pub clkdiv: ClkDiv,

    /// Clock Source register
    pub clksrc: ClkSrc,

    /// Clock Enable Register
    pub clkena: ClkEna,

    /// Timeout Register
    pub tmout: TmOut,

    /// Card Type Register
    pub ctype: CType,

    /// Block Size Register
    pub blksiz: BlkSiz,

    /// Byte Count Register
    ///
    /// Number of bytes to be transferred; should be integer multiple of Block
    /// Size for block transfers.
    ///
    /// For undefined number of byte transfers, byte count should be set to 0.
    /// When byte count is set to 0, it is responsibility of host to
    /// explicitly send stop/abort command to terminate data transfer.
    pub bytcnt: u32,

    /// Interrupt Mask Register
    pub intmask: IntMask,

    /// Command Argument Register
    ///
    /// Value indicates command argument to be passed to card
    pub cmdarg: u32,

    /// Command Register
    pub cmd: Cmd,

    /// Response Register
    #[access(ReadOnly)]
    pub resp: [u32; 4],

    /// Masked Interrupt Status Register
    #[access(ReadOnly)]
    pub mintsts: MIntSts,

    /// Raw Interrupt Status Register
    pub rintsts: RIntSts,

    /// Status Register
    #[access(ReadOnly)]
    pub status: Status,

    /// FIFO Threshold Watermark Register
    pub fifoth: FifoTh,

    /// Card Detect Register
    #[access(ReadOnly)]
    pub cdetect: CDetect,

    /// Write Protect Register
    #[access(ReadOnly)]
    pub wrtprt: WrtPrt,

    /// General Purpose Input/Output Register
    pub gpio: GPIO,

    /// Transferred CIU Card Byte Count Register
    ///
    /// Number of bytes transferred by CIU unit to card.
    ///
    /// In 32-bit or 64-bit AMBA data-bus-width modes, register should be
    /// accessed in full to avoid read-coherency problems.In 16-bit AMBA
    /// data-bus-width mode, internal 16-bit coherency register is implemented.
    /// User should first read lower 16 bits and then higher 16 bits. When
    /// reading lower 16 bits, higher 16 bits of counter are stored in temporary
    /// register. When higher 16 bits are read, data from temporary register is
    /// supplied. Both TCBCNT and TBBCNT share same coherency register. When
    /// AREA_OPTIMIZED parameter is 1, register should be read only after data
    /// transfer completes; during data transfer,register returns 0.
    #[access(ReadOnly)]
    pub tcbcnt: u32,

    /// Transferred Host to BIU-FIFO Byte Count Register
    ///
    /// Number of bytes transferred between Host/DMA memory and BIU FIFO.
    ///
    /// In 32-bit or 64-bit AMBA data-bus-width modes, register should be
    /// accessed in full to avoid read-coherency problems.In 16-bit AMBA
    /// data-bus-width mode, internal 16-bit coherency register is implemented.
    /// User should first read lower 16 bits and then higher 16 bits. When
    /// reading lower 16 bits, higher 16 bits of counter are stored in temporary
    /// register. When higher 16 bits are read, data from temporary register is
    /// supplied.
    ///
    /// Both TCBCNT and TBBCNT share same coherency register.
    #[access(ReadOnly)]
    pub tbbcnt: u32,

    /// Debounce Count Register
    pub debnce: Debnce,

    /// User ID Register
    ///
    /// User identification register; value set by user. Default reset value can
    /// be picked by user while configuring core before synthesis.
    ///
    /// Can also be used as scratch pad register by user.
    #[access(ReadOnly)]
    pub usrid: u32,

    /// Version ID Register
    ///
    /// Synopsys version identification register; register value is hard-wired.
    /// Can be read by firmware to support different versions of core.
    #[access(ReadOnly)]
    pub verid: u32,

    /// Hardware Configuration Register
    #[access(ReadOnly)]
    pub hcon: HCon,

    /// UHS-1 Register
    pub uhs: UHS,

    /// H/W Reset
    pub rst: Rst,

    _reserved0: u32,

    /// Bus Mode Register
    pub bmod: BMod,

    /// Poll Demand Register
    ///
    /// Poll Demand. If the OWN bit of a descriptor is not set, the FSM goes to
    /// the Suspend state. The host needs to write any value into this register
    /// for the IDMAC FSM to resume normal descriptor fetch operation. This is a
    /// write only register.
    #[access(WriteOnly)]
    pub pldmnd: u32,

    /// Descriptor List Base Address Register
    ///
    /// Start of Descriptor List. Contains the base address of the First
    /// Descriptor.
    ///
    /// The LSB bits [0/1/2:0] for 16/32/64-bit bus-width) are ignored and taken
    /// as all-zero by the IDMAC internally. Hence these LSB bits are read-only.
    pub dbaddr: u32,
}

/// Control Register
#[bitfield(u32, order = Msb)]
pub struct Ctrl {
    #[bits(6)]
    __: u8,
    /// Present only for the Internal DMAC configuration; else, it is reserved.
    ///
    /// * 0: The host performs data transfers through the slave interface
    /// * 1: Internal DMAC used for data transfer
    pub use_internal_dmac: bool,
    /// External open-drain pullup:
    ///
    /// * 0: Disable
    /// * 1: Enable
    ///
    /// Inverted value of this bit is output to ccmd_od_pullup_en_n port.
    /// When bit is set, command output always driven in open-drive mode;
    /// that is, DWC_mobile_storage drives either 0 or high impedance, and does
    /// not drive hard 1.
    pub enable_od_pullup: bool,
    /// Card regulator-B voltage setting; output to card_volt_b port.
    ///
    /// Optional feature; ports can be used as general-purpose outputs.
    #[bits(4)]
    pub card_voltage_b: u8,
    /// Card regulator-A voltage setting; output to card_volt_a port.
    ///
    /// Optional feature; ports can be used as general-purpose outputs.
    #[bits(4)]
    pub card_voltage_a: u8,
    #[bits(4)]
    __: u8,
    /// * 0: Interrupts not enabled in CE-ATA device (nIEN = 1 in ATA control
    ///   register)
    /// * 1: Interrupts are enabled in CE-ATA device (nIEN = 0 in ATA control
    ///   register)
    ///
    /// Software should appropriately write to this bit after power-on reset or
    /// any other reset to CE-ATA device. After reset, usually CE-ATA device
    /// interrupt is disabled (nIEN = 1). If the host enables CE-ATA device
    /// interrupt, then software should set this bit.
    pub ceata_device_interrupt: bool,
    /// * 0: Clear bit if DWC_mobile_storage does not reset the bit
    /// * 1: Send internally generated STOP after sending CCSD to CE-ATA device
    ///
    /// Always set [`Ctrl::send_auto_stop_ccsd`] and [`Ctrl::send_ccsd`] bits
    /// together; [`Ctrl::send_auto_stop_ccsd`] should not be set independently
    /// of [`Ctrl::send_ccsd`]. When set, SD/MMC automatically sends an
    /// internally-generated STOP command (CMD12) to the CE-ATA device.
    /// After sending this internally-generated STOP command, the Auto
    /// Command Done (ACD) bit in SDHOST_RINTSTS_REG is set and an interrupt
    /// is generated for the host, in case the ACD interrupt is not masked.
    /// After sending the Command Completion Signal Disable (CCSD), SD/MMC
    /// automatically clears the [`Ctrl::send_auto_stop_ccsd`] bit.
    pub send_auto_stop_ccsd: bool,
    /// * 0: Clear bit if DWC_mobile_storage does not reset the bit
    /// * 1: Send Command Completion Signal Disable (CCSD) to CE-ATA device
    ///
    /// When set, SD/MMC sends CCSD to the CE-ATA device. Software sets this bit
    /// only if the current command is expecting CCS (that is, RW_BLK), and if
    /// interrupts are enabled for the CE-ATA device. Once the CCSD pattern is
    /// sent to the device, SD/MMC automatically clears the [`Ctrl::send_ccsd`]
    /// bit. It also sets the Command Done (CD) bit in the SDHOST_RINTSTS_REG
    /// register, and generates an interrupt for the host, in case the Command
    /// Done interrupt is not masked. NOTE: Once the [`Ctrl::send_ccsd`] bit is
    /// set, it takes two card clock cycles to drive the CCSD on the CMD line.
    /// Due to this, within the boundary conditions the CCSD may be sent to the
    /// CE-ATA device, even if the device has signalled CCS.
    pub send_ccsd: bool,
    /// * 0: No change
    /// * 1: After suspend command is issued during read-transfer, software
    ///   polls card to find when suspend happened. Once suspend occurs,
    ///   software sets bit to reset data state-machine, which is waiting for
    ///   next block of data. Bit automatically clears once data state­machine
    ///   resets to idle.
    ///
    /// Used in SDIO card suspend sequence.
    pub abort_read_data: bool,
    /// * 0: No change
    /// * 1: Send auto IRQ response
    ///
    /// Bit automatically clears once response is sent.
    ///
    /// To wait for MMC card interrupts, host issues CMD40, and
    /// DWC_mobile_storage waits for interrupt response from MMC card(s). In
    /// meantime, if host wants DWC_mobile_storage to exit waiting for interrupt
    /// state, it can set this bit, at which time DWC_mobile_storage command
    /// state-machine sends CMD40 response on bus and returns to idle state.
    pub send_irq_response: bool,
    /// * 0: Clear read wait
    /// * 1: Assert read wait
    ///
    /// For sending read-wait to SDIO cards.
    pub read_wait: bool,
    /// * 0: Disable DMA transfer mode
    /// * 1: Enable DMA transfer mode
    ///
    /// Valid only if DWC_mobile_storage configured for External DMA interface.
    pub dma_enable: bool,
    /// Global interrupt enable/disable bit:
    ///
    /// * 0: Disable interrupts
    /// * 1: Enable interrupts
    ///
    /// The int port is 1 only when this bit is 1 and one or more unmasked
    /// interrupts are set.
    pub int_enable: bool,
    __: bool,
    /// * 0: No change
    /// * 1: Reset internal DMA interface control logic
    ///
    /// To reset DMA interface, firmware should set bit to 1. This bit is
    /// auto-cleared after two AHB clocks.
    pub dma_reset: bool,
    /// * 0: No change
    /// * 1: Reset to data FIFO To reset FIFO pointers
    ///
    /// To reset FIFO, firmware should set bit to 1. This bit is auto-cleared
    /// after completion of reset operation.
    ///
    /// Note: FIFO pointers will be out of reset after 2 cycles of system clocks
    /// in addition to synchronization delay (2 cycles of card clock), after the
    /// fifo_reset is cleared.
    pub fifo_reset: bool,
    /// * 0: No change
    /// * 1: Reset DWC_mobile_storage controller
    ///
    /// To reset controller, firmware should set bit to 1. This bit is
    /// auto-cleared after two AHB and two cclk_in clock cycles.
    /// This resets:
    /// * BIU/CIU interface
    /// * CIU and state machines
    /// * [`Ctrl::abort_read_data`], [`Ctrl::send_irq_response`], and
    ///   [`Ctrl::read_wait`]
    /// * [`Command::start_cmd`]
    /// Does not affect any registers or DMA interface, or FIFO or host
    /// interrupts
    pub controller_reset: bool,
}

/// Power Enable Register
#[bitfield(u32, order = Msb)]
pub struct PwrEn {
    #[bits(16)]
    __: u16,
    /// Power on/off switch for up to 16 cards; for example, power_enable[0]
    /// controls card 0. Once power is turned on, firmware should wait for
    /// regulator/switch ramp-up time before trying to initialize card.
    ///
    /// * 0 - power off
    /// * 1 - power on
    ///
    /// Only NUM_CARDS number of bits are implemented. Bit values output to
    /// card_power_en port. Optional feature; ports can be used as
    /// general-purpose outputs
    pub power_enable: u16,
}

/// Clock Divider Register
///
/// Clock division is 2*n. For example, value of 0 means divide by 2*0 = 0 (no
/// division, bypass), a value of 1 means divide by 2*1 = 2, a value of “ff”
/// means divide by 2*255 = 510, and so on.
#[bitfield(u32, order = Msb)]
pub struct ClkDiv {
    /// Clock divider-3 value.
    ///
    /// In MMC-Ver3.3-only mode, bits not implemented because only one clock
    /// divider is supported.
    pub clk_divider3: u8,
    /// Clock divider-2 value.
    ///
    /// In MMC-Ver3.3-only mode, bits not implemented because only one clock
    /// divider is supported.
    pub clk_divider2: u8,
    /// Clock divider-1 value.
    ///
    /// In MMC-Ver3.3-only mode, bits not implemented because only one clock
    /// divider is supported.
    pub clk_divider1: u8,
    /// Clock divider-0 value.
    pub clk_divider0: u8,
}

/// Clock Source register
///
/// Clock divider source for up to 16 SD cards supported. Each card has two bits
/// assigned to it. For example, bits[1:0] assigned for card-0, which maps and
/// internally routes clock divider[3:0] outputs to cclk_out[15:0] pins,
/// depending on bit value.
///
/// * 00 - Clock divider 0
/// * 01 - Clock divider 1
/// * 10 - Clock divider 2
/// * 11 - Clock divider 3
///
/// In MMC-Ver3.3-only controller, only one clock divider supported. The
/// cclk_out is always from clock divider 0, and this register is not
/// implemented.
#[bitfield(u32, order = Msb)]
pub struct ClkSrc {
    /// Clock divider source for card 15
    #[bits(2)]
    pub card15_clk_source: u8,
    /// Clock divider source for card 14
    #[bits(2)]
    pub card14_clk_source: u8,
    /// Clock divider source for card 13
    #[bits(2)]
    pub card13_clk_source: u8,
    /// Clock divider source for card 12
    #[bits(2)]
    pub card12_clk_source: u8,
    /// Clock divider source for card 11
    #[bits(2)]
    pub card11_clk_source: u8,
    /// Clock divider source for card 10
    #[bits(2)]
    pub card10_clk_source: u8,
    /// Clock divider source for card 9
    #[bits(2)]
    pub card9_clk_source: u8,
    /// Clock divider source for card 8
    #[bits(2)]
    pub card8_clk_source: u8,
    /// Clock divider source for card 7
    #[bits(2)]
    pub card7_clk_source: u8,
    /// Clock divider source for card 6
    #[bits(2)]
    pub card6_clk_source: u8,
    /// Clock divider source for card 5
    #[bits(2)]
    pub card5_clk_source: u8,
    /// Clock divider source for card 4
    #[bits(2)]
    pub card4_clk_source: u8,
    /// Clock divider source for card 3
    #[bits(2)]
    pub card3_clk_source: u8,
    /// Clock divider source for card 2
    #[bits(2)]
    pub card2_clk_source: u8,
    /// Clock divider source for card 1
    #[bits(2)]
    pub card1_clk_source: u8,
    /// Clock divider source for card 0
    #[bits(2)]
    pub card0_clk_source: u8,
}

/// Clock Enable Register
#[bitfield(u32, order = Msb)]
pub struct ClkEna {
    /// Low-power control for up to 16 SD card clocks and one MMC card clock
    /// supported.
    ///
    /// * 0 - Non-low-power mode
    /// * 1 - Low-power mode; stop clock when card in IDLE (should be normally
    ///   set to only MMC and SD memory cards; for SDIO cards, if interrupts
    ///   must be detected, clock should not be stopped).
    ///
    /// In MMC-Ver3.3-only mode, since there is only one cclk_out, only
    /// cclk_low_power[0] is used.
    pub cclk_low_power: u16,
    /// Clock-enable control for up to 16 SD card clocks and one MMC card clock
    /// supported.
    ///
    /// * 0 - Clock disabled
    /// * 1 - Clock enabled
    ///
    /// In MMC-Ver3.3-only mode, since there is only one cclk_out, only
    /// cclk_enable[0] is used.
    pub cclk_enable: u16,
}

/// Timeout Register
#[bitfield(u32, order = Msb)]
pub struct TmOut {
    /// Value for card Data Read Timeout; same value also used for Data
    /// Starvation by Host timeout. The timeout counter is started only after
    /// the card clock is stopped. Value is in number of card output clocks
    /// cclk_out of selected card.
    ///
    /// Note: The software timer should be used if the timeout value is in the
    /// order of 100 ms. In this case, read data timeout interrupt needs to
    /// be disabled.
    #[bits(24)]
    pub data_timeout: u32,
    /// Response timeout value. Value is in number of card output clocks
    /// cclk_out.
    pub response_timeout: u8,
}

/// Card Type Register
#[bitfield(u32, order = Msb)]
pub struct CType {
    /// One bit per card indicates if card is 8-bit:
    ///
    /// * 0 - Non 8-bit mode
    /// * 1 - 8-bit mode
    ///
    /// width8[15] corresponds to card[15]; width8[0] corresponds to card[0].
    pub width8: u16,
    /// One bit per card indicates if card is 1-bit or 4-bit:
    ///
    /// * 0 - 1-bit mode
    /// * 1 - 4-bit mode
    ///
    /// width4[15] corresponds to card[15], width4[0] corresponds to card[0].
    pub width4: u16,
}

/// Block Size Register
#[bitfield(u32, order = Msb)]
pub struct BlkSiz {
    __: u16,
    /// Block size
    pub block_size: u16,
}

/// Interrupt Mask Register
#[bitfield(u32, order = Msb)]
pub struct IntMask {
    /// Mask SDIO interrupts
    ///
    /// One bit for each card. sdio[15] corresponds to card[15], and
    /// sdio[0] corresponds to card[0]. When masked, SDIO interrupt
    /// detection for that card is disabled. A 0 masks an interrupt, and 1
    /// enables an interrupt.
    ///
    /// In MMC-Ver3.3-only mode, these bits are always 0.
    pub sdio: u16,
    /// End-bit error (read)/Write no CRC (EBE) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub ebe: bool,
    /// Auto command done (ACD) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub acd: bool,
    /// Start Bit Error(SBE)/Busy Complete Interrupt (BCI) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub sbe: bool,
    /// Hardware locked write error (HLE) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub hle: bool,
    /// FIFO underrun/overrun error (FRUN) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub frun: bool,
    /// Data starvation-by-host timeout (HTO) /Volt_switch_int interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub hto: bool,
    /// Data read timeout (DRTO) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub drto: bool,
    /// Response timeout (RTO) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub rto: bool,
    /// Data CRC error (DCRC) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub dcrc: bool,
    /// Response CRC error (RCRC) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub rcrc: bool,
    /// Receive FIFO data request (RXDR) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub rxdr: bool,
    /// Transmit FIFO data request (TXDR) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub txdr: bool,
    /// Data transfer over (DTO) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub dto: bool,
    /// Command done (CD) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub cmd: bool,
    /// Response error (RE) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub re: bool,
    /// Card detect (CD) interrupt enable.
    ///
    /// Value of 0 masks interrupt; value of 1 enables interrupt.
    pub cd: bool,
}

/// Command Register
#[bitfield(u32, order = Msb)]
pub struct Cmd {
    /// Start command. Once command is taken by CIU, bit is cleared.
    ///
    /// When bit is set, host should not attempt to write to any command
    /// registers. If write is attempted, hardware lock error is set in raw
    /// interrupt register.
    ///
    /// Once command is sent and response is received from SD_MMC_CEATA cards,
    /// Command Done bit is set in raw interrupt register.
    #[bits(default = true)]
    pub start_cmd: bool,
    __: bool,
    /// Use Hold Register
    ///
    /// * 0 - CMD and DATA sent to card bypassing HOLD Register
    /// * 1 - CMD and DATA sent to card through the HOLD Register
    pub use_hold_reg: bool,
    /// Voltage switch bit
    ///
    /// * 0 - No voltage switching
    /// * 1 - Voltage switching enabled; must be set for CMD11 only
    pub volt_switch: bool,
    /// Boot Mode
    ///
    /// * 0 - Mandatory Boot operation
    /// * 1 - Alternate Boot operation
    pub boot_mode: bool,
    /// Disable Boot.
    ///
    /// When software sets this bit along with start_cmd, CIU
    /// terminates the boot operation. Do NOT set [`Cmd::disable_boot`] and
    /// [`Cmd::enable_boot`] together.
    pub disable_boot: bool,
    /// Expect Boot Acknowledge.
    ///
    /// When Software sets this bit along with [`Cmd::enable_boot`], CIU
    /// expects a boot acknowledge start pattern of 0-1-0 from the selected
    /// card.
    pub expect_boot_ack: bool,
    /// Enable Boot this bit should be set only for mandatory boot mode.
    ///
    /// When Software sets this bit along with start_cmd, CIU starts the boot
    /// sequence for the corresponding card by asserting the CMD line low. Do
    /// NOT set [`Cmd::disable_boot`] and [`Cmd::enable_boot`] together.
    pub enable_boot: bool,
    /// * 0 - Interrupts are not enabled in CE-ATA device (nIEN = 1 in ATA
    ///   control register), or command does not expect CCS from device
    /// * 1 - Interrupts are enabled in CE-ATA device (nIEN = 0), and RW_BLK
    ///   command expects command completion signal from CE-ATA device
    ///
    /// If the command expects Command Completion Signal (CCS) from the CE-ATA
    /// device, the software should set this control bit.DWC_mobile_storage sets
    /// Data Transfer Over (DTO) bit in RINTSTS register and generates interrupt
    /// to host if Data Transfer Over interrupt is not masked.
    pub ccs_expected: bool,
    /// * 0 - Host is not performing read access (RW_REG or RW_BLK) towards
    ///   CE-ATA device
    /// * 1 - Host is performing read access (RW_REG or RW_BLK) towards CE-ATA
    ///   device
    ///
    /// Software should set this bit to indicate that CE-ATA device is being
    /// accessed for read transfer. This bit is used to disable read data
    /// timeout indication while performing CE-ATA read transfers.Maximum value
    /// of I/O transmission delay can be no less than 10 seconds.
    /// DWC_mobile_storage should not indicate read data timeout while waiting
    /// for data from CE-ATA device.
    pub read_ceata_device: bool,
    /// * 0 - Normal command sequence
    /// * 1 - Do not send commands, just update clock register value into card
    ///   clock domain
    ///
    /// Following register values transferred into card clock domain:
    /// [`ClkDiv`], [`ClkSrc`], [`ClkEna`]. Changes card clocks (change
    /// frequency, truncate off or on, and set low-frequency mode); provided
    /// in order to change clock frequency or stop clock without having to
    /// send command to cards. During normal command sequence, when
    /// [`Cmd::update_clock_registers_only`] = 0, following control registers
    /// are transferred from BIU to CIU: [`Cmd`], [`CmdArg`], [`TmOut`],
    /// [`ClkType`], [`BlkSiz`], [`BytCnt`]. CIU uses new register values for
    /// new command sequence to card(s).When bit is set, there are no Command
    /// Done interrupts because no command is sent to SD_MMC_CEATA cards.
    pub update_clock_registers_only: bool,
    /// Card number in use. Represents physical slot number of card being
    /// accessed.
    ///
    /// In MMC-Ver3.3-only mode, up to 30 cards are supported; in SD-only mode,
    /// up to 16 cards are supported. Registered version of this is reflected on
    /// dw_dma_card_num and ge_dma_card_num ports, which can be used to create
    /// separate DMA requests, if needed.
    ///
    /// In addition, in SD mode this is used to mux or demux signals from
    /// selected card because each card is interfaced to DWC_mobile_storage
    /// by separate bus.
    #[bits(5)]
    pub card_number: u16,
    /// * 0 - Do not send initialization sequence (80 clocks of 1) before
    ///   sending this command
    /// * 1 - Send initialization sequence before sending this command
    ///
    /// After power on, 80 clocks must be sent to card for initialization before
    /// sending any commands to card. Bit should be set while sending first
    /// command to card so that controller will initialize clocks before sending
    /// command to card. This bit should not be set for either of the boot modes
    /// (alternate or mandatory).
    pub send_initialization: bool,
    /// * 0 - Neither stop nor abort command to stop current data transfer in
    ///   progress. If abort is sent to function-number currently selected or
    ///   not in data-transfer mode, then bit should be set to 0.
    /// * 1 - Stop or abort command intended to stop current data transfer in
    ///   progress.
    ///
    /// When open-ended or predefined data transfer is in progress, and host
    /// issues stop or abort command to stop data transfer, bit should be set so
    /// that command/data state-machines of CIU can return correctly to idle
    /// state. This is also applicable for Boot mode transfers. To Abort boot
    /// mode, this bit should be set along with CMD[26] = disable_boot.
    pub stop_abort_cmd: bool,
    /// * 0 - Send command at once, even if previous data transfer has not
    /// completed
    /// * 1 - Wait for previous data transfer completion before sending command
    ///
    /// The [`Cmd::wait_prvdata_complete`] = 0 option typically used to query
    /// status of card during data transfer or to stop current data
    /// transfer; card_number should be same as in previous command.
    #[bits(default = true)]
    pub wait_prvdata_complete: bool,
    /// * 0 - No stop command sent at end of data transfer
    /// * 1 - Send stop command at end of data transfer
    ///
    /// When set, DWC_mobile_storage sends stop command to SD_MMC_CEATA cards at
    /// end of data transfer.
    ///  * when send_auto_stop bit should be set, since some data transfers do
    ///    not need explicit stop commands
    ///  * open-ended transfers that software should explicitly send to stop
    ///    command
    ///
    /// Additionally, when “resume” is sent to resume  suspended memory access
    /// of SD-Combo card  bit should be set correctly if suspended data transfer
    /// needs send_auto_stop.
    ///
    /// Don't care if no data expected from card.
    pub send_auto_stop: bool,
    /// * 0 - Block data transfer command
    /// * 1 - Stream data transfer command
    ///
    /// Don’t care if no data expected.
    pub transfer_mode: bool,
    /// * 0 - Read from card
    /// * 1 - Write to card
    ///
    /// Don’t care if no data expected from card.
    pub read_write: bool,
    /// * 0 - No data transfer expected (read/write)
    /// * 1 - Data transfer expected (read/write)
    pub data_expected: bool,
    /// * 0 - Do not check response CRC
    /// * 1 - Check response CRC
    ///
    /// Some of command responses do not return valid CRC bits. Software should
    /// disable CRC checks for those commands in order to disable CRC checking
    /// by controller.
    pub check_response_crc: bool,
    /// * 0 - Short response expected from card
    /// * 1 - Long response expected from card
    pub response_length: bool,
    /// * 0 - No response expected from card
    /// * 1 - Response expected from card
    pub response_expect: bool,
    #[bits(6)]
    /// Command index
    pub cmd_index: u8,
}

/// Masked Interrupt Status Register
#[bitfield(u32, order = Msb)]
pub struct MIntSts {
    /// Interrupt from SDIO card; one bit for each card. sdio[15]
    /// corresponds to Card[15], and sdio[0] is for Card[0]. SDIO
    /// interrupt for card enabled only if corresponding sdio_int_mask bit
    /// is set in Interrupt mask register (mask bit 1 enables interrupt; 0
    /// masks interrupt).
    ///
    /// * 0 - No SDIO interrupt from card
    /// * 1 - SDIO interrupt from card
    ///
    /// In MMC-Ver3.3-only mode, bits always 0.
    pub sdio: u16,
    /// Interrupt enabled only if corresponding bit in interrupt mask register
    /// is set.
    pub end_bit_error: bool,
    /// Auto command done (ACD)
    pub auto_command_done: bool,
    /// Start bit error (SBE)/Busy complete interrupt (BCI)
    pub start_bit_error: bool,
    /// Hardware locked write error (HLE)
    pub hardware_locked_write: bool,
    /// FIFO underrun/overrun error (FRUN)
    pub fifo_under_over_run: bool,
    /// Data starvation by host timeout (HTO)/Volt_switch_int
    pub host_timeout: bool,
    /// Data read timeout (DRTO)
    pub data_read_timeout: bool,
    /// Response timeout (RTO)
    pub response_timeout: bool,
    /// Data CRC error (DCRC)
    pub data_crc_error: bool,
    /// Response CRC error (RCRC)
    pub response_crc_error: bool,
    /// Receive FIFO data request (RXDR)
    pub receive_fifo_data_request: bool,
    /// Transmit FIFO data request (TXDR)
    pub transmit_fifo_data_request: bool,
    /// Data transfer over (DTO)
    pub data_transfer_over: bool,
    /// Command done (CD)
    pub command_done: bool,
    /// Response error (RE)
    pub response_error: bool,
    /// Card detect (CD)
    pub card_detect: bool,
}

/// Raw Interrupt Status Register
#[bitfield(u32, order = Msb)]
pub struct RIntSts {
    /// Interrupt from SDIO card; one bit for each card. sdio[15]
    /// corresponds to Card[15], and sdio[0] is for Card[0].
    /// Writes to these bits clear them. Value of 1 clears bit and 0 leaves
    /// bit intact.
    ///
    /// * 0 - No SDIO interrupt from card
    /// * 1 - SDIO interrupt from card
    ///
    /// In MMC-Ver3.3-only mode, bits always 0.
    ///
    /// Bits are logged regardless of interrupt-mask status.
    pub sdio: u16,
    /// End-bit error (read)/write no CRC (EBE)
    pub end_bit_error: bool,
    /// Auto command done (ACD)
    pub auto_command_done: bool,
    /// Start bit error (SBE)/Busy complete interrupt (BCI)
    pub start_bit_error: bool,
    /// Hardware locked write error (HLE)
    pub hardware_locked_write: bool,
    /// FIFO underrun/overrun error (FRUN)
    pub fifo_under_over_run: bool,
    /// Data starvation by host timeout (HTO)/Volt_switch_int
    pub host_timeout: bool,
    /// Data read timeout (DRTO)
    pub data_read_timeout: bool,
    /// Response timeout (RTO)
    pub response_timeout: bool,
    /// Data CRC error (DCRC)
    pub data_crc_error: bool,
    /// Response CRC error (RCRC)
    pub response_crc_error: bool,
    /// Receive FIFO data request (RXDR)
    pub receive_fifo_data_request: bool,
    /// Transmit FIFO data request (TXDR)
    pub transmit_fifo_data_request: bool,
    /// Data transfer over (DTO)
    pub data_transfer_over: bool,
    /// Command done (CD)
    pub command_done: bool,
    /// Response error (RE)
    pub response_error: bool,
    /// Card detect (CD)
    pub card_detect: bool,
}

impl RIntSts {
    pub fn error(&self) -> bool {
        self.response_timeout()
            || self.data_read_timeout()
            || self.start_bit_error()
            || self.end_bit_error()
            || self.data_crc_error()
            || self.response_crc_error()
            || self.response_error()
            || self.hardware_locked_write()
    }
}

/// Status Register
#[bitfield(u32, order = Msb)]
pub struct Status {
    /// DMA request signal state; either dw_dma_req or ge_dma_req, depending on
    /// DW-DMA or Generic-DMA selection.
    pub dma_req: bool,
    /// DMA acknowledge signal state; either dw_dma_ack or ge_dma_ack, depending
    /// on DW-DMA or Generic-DMA selection.
    pub dma_ack: bool,
    /// FIFO count Number of filled locations in FIFO
    #[bits(13)]
    pub fifo_count: u16,
    /// Index of previous response, including any auto-stop sent by core
    #[bits(6)]
    pub response_index: u8,
    /// Data transmit or receive state-machine is busy
    pub data_state_mc_busy: bool,
    /// Inverted version of raw selected card_data[0]
    ///
    /// * 0 - card data not busy
    /// * 1 - card data busy
    pub data_busy: bool,
    /// Raw selected card_data[3]; checks whether card is present
    ///
    /// * 0 - card not present
    /// * 1 - card present
    pub data_3_status: bool,
    /// Command FSM states:
    ///
    /// * 0 - Idle
    /// * 1 - Send init sequence
    /// * 2 - Tx cmd start bit
    /// * 3 - Tx cmd tx bit
    /// * 4 - Tx cmd index + arg
    /// * 5 - Tx cmd crc7
    /// * 6 - Tx cmd end bit
    /// * 7 - Rx resp start bit
    /// * 8 - Rx resp IRQ response
    /// * 9 - Rx resp tx bit
    /// * 10 - Rx resp cmd idx
    /// * 11 - Rx resp data
    /// * 12 - Rx resp crc7
    /// * 13 - Rx resp end bit
    /// * 14 - Cmd path wait NCC
    /// * 15 - Wait; CMD-to-response turnaround
    ///
    /// NOTE: The command FSM state is represented using 19 bits.
    /// [`Status::command_fsm_states`] has 4 bits to represent the command FSM
    /// states. Using these 4 bits, only 16 states can be represented. Thus
    /// three states cannot be represented in [`Status::command_fsm_states`].
    /// The three states that are not represented in
    /// [`Status::command_fsm_states`] are:
    ///
    ///  * 16 - Wait for CCS
    ///  * 17 - Send CCSD
    ///  * 18 - Boot Mode
    ///
    /// Due to this, while command FSM is in “Wait for CCS state” or “Send CCSD”
    /// or “Boot Mode”, [`Status::command_fsm_states`] indicates status as 0.
    #[bits(4)]
    pub command_fsm_states: u8,
    /// FIFO is full status
    pub fifo_full: bool,
    /// FIFO is empty status
    pub fifo_empty: bool,
    /// FIFO reached Transmit watermark level; not qualified with data transfer.
    pub fifo_tx_watermark: bool,
    /// FIFO reached Receive watermark level; not qualified with data transfer.
    pub fifo_rx_watermark: bool,
}

/// FIFO Threshold Watermark Register
#[bitfield(u32, order = Msb)]
pub struct FifoTh {
    __: bool,
    /// Burst size of multiple transaction; should be programmed same as DW-DMA
    /// controller multiple-transaction-size SRC/DEST_MSIZE.
    ///
    /// * 000 - 1 transfers
    /// * 001 - 4
    /// * 010 - 8
    /// * 011 - 16
    /// * 100 - 32
    /// * 101 - 64
    /// * 110 - 128
    /// * 111 - 256
    ///
    /// The units for transfers is the H_DATA_WIDTH parameter. A single transfer
    /// (dw_dma_single assertion in case of Non DW DMA interface) would be
    /// signalled based on this value. Value should be sub-multiple of
    /// (RX_WMark+1)\*(F_DATA_WIDTH/H_DATA_WIDTH) and
    /// (FIFO_DEPTH-TX_WMark)\*(F_DATA_WIDTH/H_DATA_WIDTH)
    ///
    /// For example, if FIFO_DEPTH = 16, FDATA_WIDTH == H_DATA_WIDTH
    ///
    /// Allowed combinations for MSize and TX_WMark are:
    /// * MSize = 1, TX_WMARK = 1-15
    /// * MSize = 4, TX_WMark = 8
    /// * MSize = 4, TX_WMark = 4
    /// * MSize = 4, TX_WMark = 12
    /// * MSize = 8, TX_WMark = 8
    /// * MSize = 8, TX_WMark = 4
    ///
    /// Allowed combinations for MSize and RX_WMark are:
    /// * MSize = 1, RX_WMARK = 0-14
    /// * MSize = 4, RX_WMark = 3
    /// * MSize = 4, RX_WMark = 7
    /// * MSize = 4, RX_WMark = 11
    /// * MSize = 8, RX_WMark = 7
    ///
    /// Recommended:
    /// MSize = 8, TX_WMark = 8, RX_WMark = 7
    #[bits(3)]
    pub dw_dma_multiple_transaction_size: u8,
    /// FIFO threshold watermark level when receiving data to card.
    ///
    /// When FIFO data count reaches greater than this number,DMA/FIFO request
    /// is raised. During end of packet, request is generated regardless of
    /// threshold programming in order to complete any remaining data.
    ///
    /// In non-DMA mode, when receiver FIFO threshold (RXDR) interrupt is
    /// enabled, then interrupt is generated instead of DMA request.
    ///
    /// During end of packet, interrupt is not generated if threshold
    /// programming is larger than any remaining data. It is responsibility of
    /// host to read remaining bytes on seeing Data Transfer Done interrupt.
    ///
    /// In DMA mode, at end of packet, even if remaining bytes are less than
    /// threshold, DMA request does single transfers to flush out any remaining
    /// bytes before Data Transfer Done interrupt is set.
    ///
    /// 12 bits-1 bit less than FIFO-count of status register, which is 13 bits.
    ///
    /// Limitation: RX_WMark <= FIFO_DEPTH-2
    ///
    /// Recommended: (FIFO_DEPTH/2) - 1; (means greater than (FIFO_DEPTH/2) - 1)
    ///
    /// NOTE: In DMA mode during CCS time-out, the DMA does not generate the
    /// request at the end of packet, even if remaining bytes are less than
    /// threshold. In this case, there will be some data left in the FIFO. It is
    /// the responsibility of the application to reset the FIFO after the CCS
    /// timeout.
    #[bits(12)]
    pub rx_watermark: u16,
    #[bits(4)]
    __: u8,
    /// FIFO threshold watermark level when transmitting data to card.
    ///
    /// When FIFO data count is less than or equal to this number,DMA/FIFO
    /// request is raised. If Interrupt is enabled, then interrupt occurs.
    /// During end of packet, request or interrupt is generated,regardless of
    /// threshold programming.
    ///
    /// In non-DMA mode, when transmit FIFO threshold (TXDR) interrupt is
    /// enabled, then interrupt is generated instead of DMA request. During end
    /// of packet, on last interrupt, host is responsible for filling FIFO with
    /// only required remaining bytes (not before FIFO is full or after CIU
    /// completes data transfers, because FIFO may not be empty).
    ///
    /// In DMA mode, at end of packet, if last transfer is less than burst size,
    /// DMA controller does single cycles until required bytes are transferred.
    ///
    /// 12 bits-1 bit less than FIFO-count of status register, which is 13 bits.
    ///
    /// Limitation: TX_WMark >= 1;
    ///
    /// Recommended: FIFO_DEPTH/2; (means less than or equal to FIFO_DEPTH/2)
    #[bits(12)]
    pub tx_watermark: u16,
}

/// Card Detect Register
#[bitfield(u32, order = Msb)]
pub struct CDetect {
    __: u16,
    /// 0 represents presence of card.
    ///
    /// Only NUM_CARDS number of bits are implemented.
    pub card_detect_n: u16,
}

/// Write Protect Register
#[bitfield(u32, order = Msb)]
pub struct WrtPrt {
    __: u16,
    /// 1 represents write protection.
    ///
    /// Only NUM_CARDS number of bits are implemented.
    pub write_protect: u16,
}

/// General Purpose Input/Output Register
#[bitfield(u32, order = Msb)]
pub struct GPIO {
    __: u8,
    /// Value needed to be driven to gpo pins; this portion of register is
    /// read/write. Valid only when AREA_OPTIMIZED parameter is 0.
    pub gpo: u16,
    /// Value on gpi input ports; this portion of register is read-only. Valid
    /// only when AREA_OPTIMIZED parameter is 0.
    #[bits(access = RO)]
    pub gpi: u8,
}

/// Debounce Count Register
#[bitfield(u32, order = Msb)]
pub struct Debnce {
    __: u8,
    /// Number of host clocks (clk) used by debounce filter logic; typical
    /// debounce time is 5-25 ms.
    #[bits(24)]
    pub debounce_count: u32,
}

/// Hardware Configuration Register
#[bitfield(u32, order = Msb)]
pub struct HCon {
    #[bits(4)]
    __: u8,
    /// Address configuration
    ///
    /// * 0 - 32-bit addressing supported
    /// * 1 - 64-bit addressing supported
    pub addr_config: bool,
    /// Area Optimization
    ///
    /// * 0 - No area optimization
    /// * 1 - Area optimization
    pub area_opt: bool,
    /// NUM_CLK_DIVIDER - 1
    #[bits(2)]
    pub num_clk_dic: u8,
    /// Set Clock False Path
    ///
    /// * 0 - No false path
    /// * 1 - False path set
    pub false_path: bool,
    /// Implement HOLD register
    ///
    /// * 0 - No HOLD register
    /// * 1 - HOLD register
    pub hold_reg: bool,
    /// FIFO Ram Inside
    ///
    /// * 0 - Outside
    /// * 1 - Inside
    pub fifo_ram_ins: bool,
    /// Generic DMA Data Width
    ///
    /// * 000 - 16 bits
    /// * 001 - 32 bits
    /// * 010 - 64 bits
    /// * others - reserved
    #[bits(3)]
    pub ge_dma_data_width: u8,
    /// DMA Interface
    ///
    /// * 0 - None
    /// * 1 - DW DMA
    /// * 2 - Generic DMA
    /// * 3 - Non DW DMA
    #[bits(2)]
    pub dma_if: u8,
    /// H Address Width
    ///
    /// * 00 to 7  reserved
    /// * 8  9 bits
    /// * 9  10 bits
    /// * …
    /// * 31  32 bits
    /// * 32 to 63  reserved
    #[bits(6)]
    pub h_addr_width: u8,
    /// H Data Width
    /// * 000 - 16 bits
    /// * 001 - 32 bits
    /// * 010 - 64 bits
    /// * others - reserved
    #[bits(3)]
    pub h_data_width: u8,
    /// Bus type
    ///
    /// * 0 - APB bus
    /// * 1 - AHB bus
    pub bus_type: bool,
    /// NUM_CARD - 1
    #[bits(5)]
    pub num_card: u8,
    /// Card type
    ///
    /// * 0 - MMC only
    /// * 1 - SD MMC
    pub card_type: bool,
}

/// UHS-1 Register
#[bitfield(u32, order = Msb)]
pub struct UHS {
    /// DDR mode. These bits indicate DDR mode of operation to the core for the
    /// data transfer.
    ///
    /// * 0 - Non-DDR mode
    /// * 1 - DDR mode
    ///
    /// ddr[0] should be set for card number 0, ddr[1] for card number 1 and
    /// so on.
    pub ddr: u16,
    /// High Voltage mode. Determines the voltage fed to the buffers by an
    /// external voltage regulator.
    ///
    /// * 0 - Buffers supplied with 3.3V Vdd
    /// * 1 - Buffers supplied with 1.8V Vdd
    ///
    /// These bits function as the output of the host controller and are fed to
    /// an external voltage regulator. The voltage regulator must switch the
    /// voltage of the buffers of a particular card to either 3.3V or 1.8V,
    /// depending on the value programmed in the register.
    ///
    /// volt[0] should be set to 1 for card number 0 in order to make it operate
    /// for 1.8V.
    pub volt: u16,
}

/// H/W Reset
#[bitfield(u32, order = Msb)]
pub struct Rst {
    __: u16,
    /// Hardware reset.
    ///
    /// * 1 - Active mode
    /// * 0 - Reset
    ///
    /// These bits cause the cards to enter pre-idle state, which requires them
    /// to be re-initialized.
    ///
    /// * card_reset[0] should be set to 1’b0 to reset card number 0
    /// * card_reset[15] should be set to 1'b0 to reset card number 15.
    ///
    /// The number of bits implemented is restricted to NUM_CARDS.
    pub card_reset: u16,
}

/// Bus Mode Register
#[bitfield(u32, order = Msb)]
pub struct BMod {
    #[bits(21)]
    __: u32,
    /// Programmable Burst Length. These bits indicate the maximum number of
    /// beats to be performed in one IDMAC transaction. The IDMAC will always
    /// attempt to burst as specified in PBL each time it starts a Burst
    /// transfer on the host bus. The permissible values are 1, 4, 8, 16, 32,
    /// 64, 128 and 256. This value is the mirror of MSIZE of FIFOTH register.
    /// In order to change this value, write the required value to FIFOTH
    /// register. This is an encode value as follows:
    ///
    /// * 000 - 1 transfers
    /// * 001 - 4 transfers
    /// * 010 - 8 transfers
    /// * 011 - 16 transfers
    /// * 100 - 32 transfers
    /// * 101 - 64 transfers
    /// * 110 - 128 transfers
    /// * 111 - 256 transfers
    ///
    /// Transfer unit is either 16, 32, or 64 bits, based on HDATA_WIDTH.
    ///
    /// PBL is a read-only value and is applicable only for Data Access; it does
    /// not apply to descriptor accesses.
    #[bits(3, access = RO)]
    pub pbl: u8,
    /// IDMAC Enable. When set, the IDMAC is enabled.
    ///
    /// DE is read/write.
    pub de: bool,
    /// Descriptor Skip Length. Specifies the number of HWord/Word/Dword
    /// (depending on 16/32/64-bit bus) to skip between two unchained
    /// descriptors. This is applicable only for dual buffer structure.
    ///
    /// DSL is read/write.
    #[bits(5)]
    pub dsl: u8,
    /// Fixed Burst. Controls whether the AHB Master interface performs fixed
    /// burst transfers or not. When set,the AHB will use only SINGLE, INCR4,
    /// INCR8 or INCR16 during start of normal burst transfers.When reset,the
    /// AHB will use SINGLE and INCR burst transfer operations.
    ///
    /// FB is read/write.
    pub fb: bool,
    /// Software Reset. When set,the DMA Controller resets all its internal
    /// registers.
    ///
    /// SWR is read/write. It is automatically cleared after 1 clock cycle.
    pub swr: bool,
}

// TODO: there are more registers

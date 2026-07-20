use rdif_eth::{OwnerInitInput, OwnerInitSchedule};

use crate::{ChipVersion, Error, registers::Rtl8125OwnerInitRegs};

const INIT_BATCH_LIMIT: usize = 32;
const INIT_POLL_INTERVAL_NS: u64 = 10_000;
const RESET_TIMEOUT_NS: u64 = 100_000_000;
const REGISTER_TIMEOUT_NS: u64 = 10_000_000;
const FIFO_SETTLE_NS: u64 = 10_000;
const FIFO_TIMEOUT_NS: u64 = 20_000_000;
const SHORT_SETTLE_NS: u64 = 1_000;

const EEE_TXIDLE_TIMER_VALUE: u16 = 1500 + 14 + 0x20;
const CSI_PCIE_CTRL: u32 = 0x070c;
const CSI_PCIE_ZRXDC_NONCOMPL: u32 = 1 << 20;
const CSI_L1_ENTRY_LATENCY: u32 = 0x0719;
const CSI_L1_ENTRY_LATENCY_DEFAULT: u8 = 0x27;
const OCP_STD_PHY_BASE: u32 = 0xa400;

const MII_BMCR: u8 = 0x00;
const MII_CTRL1000: u8 = 0x09;
const BMCR_ANRESTART: u16 = 0x0200;
const BMCR_ANENABLE: u16 = 0x1000;
const BMCR_AUTONEG_MASK: u16 = BMCR_ANENABLE | BMCR_ANRESTART;
const ADVERTISE_1000HALF: u16 = 0x0100;
const ADVERTISE_1000FULL: u16 = 0x0200;
const ADVERTISE_1000_MASK: u16 = ADVERTISE_1000HALF | ADVERTISE_1000FULL;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InitState {
    Discovered,
    ResetPending { deadline_ns: u64 },
    FifoSettling { ready_at_ns: u64, deadline_ns: u64 },
    LinkListBefore { deadline_ns: u64 },
    LinkListAfter { deadline_ns: u64 },
    VendorClear { offset: usize },
    AspmSettling { ready_at_ns: u64 },
    EphyStart { index: usize },
    EphyReadPending { index: usize, deadline_ns: u64 },
    EphyWritePending { index: usize, deadline_ns: u64 },
    CommonPrefix { index: usize },
    CsiStart { index: usize },
    CsiReadPending { index: usize, deadline_ns: u64 },
    CsiWritePending { index: usize, deadline_ns: u64 },
    CommonSuffix { index: usize },
    PulseSettling { ready_at_ns: u64 },
    OcpSettling { deadline_ns: u64 },
    PhyStart { index: usize },
    PhyReadPending { index: usize, deadline_ns: u64 },
    PhyWritePending { index: usize, deadline_ns: u64 },
    Finalize,
    Ready,
    Failed,
}

pub(crate) struct Rtl8125InitMachine {
    state: InitState,
    chip: ChipVersion,
    mac: [u8; 6],
}

pub(crate) struct Rtl8125InitReady {
    pub(crate) chip: ChipVersion,
    pub(crate) mac: [u8; 6],
}

pub(crate) enum Rtl8125InitProgress {
    Ready(Rtl8125InitReady),
    Pending(OwnerInitSchedule),
    Failed(Error),
}

impl Rtl8125InitMachine {
    pub(crate) const fn new() -> Self {
        Self {
            state: InitState::Discovered,
            chip: ChipVersion::Unknown(0),
            mac: [0; 6],
        }
    }

    pub(crate) fn poll(
        &mut self,
        regs: &Rtl8125OwnerInitRegs,
        input: OwnerInitInput,
        dma_mask: u64,
        tx_base: u64,
        rx_base: u64,
    ) -> Rtl8125InitProgress {
        let _captured_event = input.event;
        for _ in 0..INIT_BATCH_LIMIT {
            match self.step(regs, input.now_ns, dma_mask, tx_base, rx_base) {
                Step::Continue => {}
                Step::Pending(schedule) => return Rtl8125InitProgress::Pending(schedule),
                Step::Ready => {
                    return Rtl8125InitProgress::Ready(Rtl8125InitReady {
                        chip: self.chip,
                        mac: self.mac,
                    });
                }
                Step::Failed(error) => {
                    self.state = InitState::Failed;
                    return Rtl8125InitProgress::Failed(error);
                }
            }
        }
        Rtl8125InitProgress::Pending(OwnerInitSchedule::run_again())
    }

    fn step(
        &mut self,
        regs: &Rtl8125OwnerInitRegs,
        now_ns: u64,
        dma_mask: u64,
        tx_base: u64,
        rx_base: u64,
    ) -> Step {
        match self.state {
            InitState::Discovered => {
                regs.mask_interrupts();
                regs.request_reset();
                self.state = InitState::ResetPending {
                    deadline_ns: deadline(now_ns, RESET_TIMEOUT_NS),
                };
                Step::Pending(wait_again(now_ns, deadline(now_ns, RESET_TIMEOUT_NS)))
            }
            InitState::ResetPending { deadline_ns } => {
                if regs.reset_pending() {
                    return pending_or_timeout(now_ns, deadline_ns, Error::ResetTimeout);
                }
                regs.mask_interrupts();
                self.chip = chip_version_from_xid(((regs.read_tx_config() >> 20) & 0x0fcf) as u16);
                let backup = regs.read_backup_mac();
                let mac = if valid_unicast_mac(backup) {
                    backup
                } else {
                    regs.read_mac()
                };
                if !valid_unicast_mac(mac) {
                    return Step::Failed(Error::InvalidMacAddress);
                }
                regs.write_mac(mac);
                self.mac = mac;
                regs.enable_rxdv_gate();
                regs.disable_tx_rx();
                self.state = InitState::FifoSettling {
                    ready_at_ns: deadline(now_ns, FIFO_SETTLE_NS),
                    deadline_ns: deadline(now_ns, FIFO_TIMEOUT_NS),
                };
                Step::Pending(OwnerInitSchedule::wait_until(deadline(
                    now_ns,
                    FIFO_SETTLE_NS,
                )))
            }
            InitState::FifoSettling {
                ready_at_ns,
                deadline_ns,
            } => {
                if now_ns < ready_at_ns {
                    return Step::Pending(OwnerInitSchedule::wait_until(ready_at_ns));
                }
                if !regs.rxtx_empty() {
                    return pending_or_timeout(
                        now_ns,
                        deadline_ns,
                        Error::HardwareTimeout {
                            operation: "RX/TX FIFO empty",
                        },
                    );
                }
                regs.clear_now_is_oob();
                regs.mac_ocp_modify(0xe8de, 1 << 14, 0);
                self.state = InitState::LinkListBefore {
                    deadline_ns: deadline(now_ns, REGISTER_TIMEOUT_NS),
                };
                Step::Continue
            }
            InitState::LinkListBefore { deadline_ns } => {
                if !regs.link_list_ready() {
                    return pending_or_timeout(
                        now_ns,
                        deadline_ns,
                        Error::HardwareTimeout {
                            operation: "link-list FIFO ready before programming",
                        },
                    );
                }
                regs.mac_ocp_write(0xc0aa, 0x07d0);
                regs.mac_ocp_write(0xc0a6, 0x0150);
                regs.mac_ocp_write(0xc01e, 0x5555);
                self.state = InitState::LinkListAfter {
                    deadline_ns: deadline(now_ns, REGISTER_TIMEOUT_NS),
                };
                Step::Continue
            }
            InitState::LinkListAfter { deadline_ns } => {
                if !regs.link_list_ready() {
                    return pending_or_timeout(
                        now_ns,
                        deadline_ns,
                        Error::HardwareTimeout {
                            operation: "link-list FIFO ready after programming",
                        },
                    );
                }
                self.state = InitState::VendorClear { offset: 0x0a00 };
                Step::Continue
            }
            InitState::VendorClear { offset } => {
                if offset < 0x0b00 {
                    regs.write_vendor_u32(offset, 0);
                    self.state = InitState::VendorClear { offset: offset + 4 };
                    Step::Continue
                } else {
                    regs.mac_ocp_modify(0xe092, 0x00ff, 0);
                    regs.set_aspm_clkreq(false);
                    let ready_at_ns = deadline(now_ns, SHORT_SETTLE_NS);
                    self.state = InitState::AspmSettling { ready_at_ns };
                    Step::Pending(OwnerInitSchedule::wait_until(ready_at_ns))
                }
            }
            InitState::AspmSettling { ready_at_ns } => {
                if now_ns < ready_at_ns {
                    return Step::Pending(OwnerInitSchedule::wait_until(ready_at_ns));
                }
                self.state = InitState::EphyStart { index: 0 };
                Step::Continue
            }
            InitState::EphyStart { index } => {
                let program = ephy_program(self.chip);
                let Some(entry) = program.get(index).copied() else {
                    self.state = InitState::CommonPrefix { index: 0 };
                    return Step::Continue;
                };
                regs.start_ephy_read(u32::from(entry.offset));
                self.state = InitState::EphyReadPending {
                    index,
                    deadline_ns: deadline(now_ns, REGISTER_TIMEOUT_NS),
                };
                Step::Pending(wait_again(now_ns, deadline(now_ns, REGISTER_TIMEOUT_NS)))
            }
            InitState::EphyReadPending { index, deadline_ns } => {
                if !regs.ephy_read_ready() {
                    return pending_or_timeout(
                        now_ns,
                        deadline_ns,
                        Error::HardwareTimeout {
                            operation: "EPHY read",
                        },
                    );
                }
                let entry = ephy_program(self.chip)[index];
                let value = (regs.read_ephy_data() & !entry.mask) | entry.bits;
                regs.start_ephy_write(u32::from(entry.offset), value);
                self.state = InitState::EphyWritePending {
                    index,
                    deadline_ns: deadline(now_ns, REGISTER_TIMEOUT_NS),
                };
                Step::Pending(wait_again(now_ns, deadline(now_ns, REGISTER_TIMEOUT_NS)))
            }
            InitState::EphyWritePending { index, deadline_ns } => {
                if !regs.ephy_write_ready() {
                    return pending_or_timeout(
                        now_ns,
                        deadline_ns,
                        Error::HardwareTimeout {
                            operation: "EPHY write",
                        },
                    );
                }
                self.state = InitState::EphyStart { index: index + 1 };
                Step::Continue
            }
            InitState::CommonPrefix { index } => {
                if let Some(operation) = COMMON_PREFIX.get(index).copied() {
                    apply_direct(regs, operation, self.chip);
                    self.state = InitState::CommonPrefix { index: index + 1 };
                    Step::Continue
                } else {
                    self.state = InitState::CsiStart { index: 0 };
                    Step::Continue
                }
            }
            InitState::CsiStart { index } => {
                let Some(operation) = CSI_PROGRAM.get(index).copied() else {
                    self.state = InitState::CommonSuffix { index: 0 };
                    return Step::Continue;
                };
                regs.start_csi_read(operation.address);
                self.state = InitState::CsiReadPending {
                    index,
                    deadline_ns: deadline(now_ns, REGISTER_TIMEOUT_NS),
                };
                Step::Pending(wait_again(now_ns, deadline(now_ns, REGISTER_TIMEOUT_NS)))
            }
            InitState::CsiReadPending { index, deadline_ns } => {
                if !regs.csi_read_ready() {
                    return pending_or_timeout(
                        now_ns,
                        deadline_ns,
                        Error::HardwareTimeout {
                            operation: "CSI read",
                        },
                    );
                }
                let operation = CSI_PROGRAM[index];
                let value = (regs.csi_read_data() & !operation.mask) | operation.set;
                regs.start_csi_write(operation.address, value);
                self.state = InitState::CsiWritePending {
                    index,
                    deadline_ns: deadline(now_ns, REGISTER_TIMEOUT_NS),
                };
                Step::Pending(wait_again(now_ns, deadline(now_ns, REGISTER_TIMEOUT_NS)))
            }
            InitState::CsiWritePending { index, deadline_ns } => {
                if !regs.csi_write_ready() {
                    return pending_or_timeout(
                        now_ns,
                        deadline_ns,
                        Error::HardwareTimeout {
                            operation: "CSI write",
                        },
                    );
                }
                self.state = InitState::CsiStart { index: index + 1 };
                Step::Continue
            }
            InitState::CommonSuffix { index } => {
                let program = common_suffix(self.chip);
                if let Some(operation) = program.get(index).copied() {
                    apply_direct(regs, operation, self.chip);
                    self.state = InitState::CommonSuffix { index: index + 1 };
                    Step::Continue
                } else {
                    regs.mac_ocp_modify(0xeb54, 0, 0x0001);
                    let ready_at_ns = deadline(now_ns, SHORT_SETTLE_NS);
                    self.state = InitState::PulseSettling { ready_at_ns };
                    Step::Pending(OwnerInitSchedule::wait_until(ready_at_ns))
                }
            }
            InitState::PulseSettling { ready_at_ns } => {
                if now_ns < ready_at_ns {
                    return Step::Pending(OwnerInitSchedule::wait_until(ready_at_ns));
                }
                regs.mac_ocp_modify(0xeb54, 0x0001, 0);
                regs.clear_vendor_u16_bits(0x1880, 0x0030);
                regs.mac_ocp_write(0xe098, 0xc302);
                self.state = InitState::OcpSettling {
                    deadline_ns: deadline(now_ns, REGISTER_TIMEOUT_NS),
                };
                Step::Continue
            }
            InitState::OcpSettling { deadline_ns } => {
                regs.start_mac_ocp_read(0xe00e);
                if regs.read_mac_ocp_data() & (1 << 13) != 0 {
                    return pending_or_timeout(
                        now_ns,
                        deadline_ns,
                        Error::HardwareTimeout {
                            operation: "MAC OCP 0xe00e settle",
                        },
                    );
                }
                if self.chip == ChipVersion::Rtl8125B {
                    regs.write_eee_txidle_timer(EEE_TXIDLE_TIMER_VALUE);
                } else {
                    regs.mac_ocp_modify(0xeb62, 0, (1 << 2) | (1 << 1));
                }
                regs.mac_ocp_modify(0xe040, 0, (1 << 1) | 1);
                regs.disable_rxdv_gate();
                self.state = InitState::PhyStart { index: 0 };
                Step::Continue
            }
            InitState::PhyStart { index } => {
                let program = phy_program(self.chip);
                let Some(operation) = program.get(index).copied() else {
                    self.state = InitState::Finalize;
                    return Step::Continue;
                };
                let address = phy_address(operation.page(), operation.register());
                match operation {
                    PhyOp::Write { value, .. } => {
                        regs.start_phy_ocp_write(address, value);
                        self.state = InitState::PhyWritePending {
                            index,
                            deadline_ns: deadline(now_ns, REGISTER_TIMEOUT_NS),
                        };
                    }
                    PhyOp::Modify { .. } => {
                        regs.start_phy_ocp_read(address);
                        self.state = InitState::PhyReadPending {
                            index,
                            deadline_ns: deadline(now_ns, REGISTER_TIMEOUT_NS),
                        };
                    }
                }
                Step::Pending(wait_again(now_ns, deadline(now_ns, REGISTER_TIMEOUT_NS)))
            }
            InitState::PhyReadPending { index, deadline_ns } => {
                if !regs.phy_ocp_read_ready() {
                    return pending_or_timeout(
                        now_ns,
                        deadline_ns,
                        Error::HardwareTimeout {
                            operation: "PHY OCP read",
                        },
                    );
                }
                let PhyOp::Modify {
                    page,
                    register,
                    mask,
                    set,
                } = phy_program(self.chip)[index]
                else {
                    return Step::Failed(Error::HardwareTimeout {
                        operation: "PHY program state mismatch",
                    });
                };
                let value = (regs.read_phy_ocp_data() & !mask) | set;
                regs.start_phy_ocp_write(phy_address(page, register), value);
                self.state = InitState::PhyWritePending {
                    index,
                    deadline_ns: deadline(now_ns, REGISTER_TIMEOUT_NS),
                };
                Step::Pending(wait_again(now_ns, deadline(now_ns, REGISTER_TIMEOUT_NS)))
            }
            InitState::PhyWritePending { index, deadline_ns } => {
                if !regs.phy_ocp_write_ready() {
                    return pending_or_timeout(
                        now_ns,
                        deadline_ns,
                        Error::HardwareTimeout {
                            operation: "PHY OCP write",
                        },
                    );
                }
                self.state = InitState::PhyStart { index: index + 1 };
                Step::Continue
            }
            InitState::Finalize => {
                regs.program_controller(dma_mask, tx_base, rx_base);
                self.state = InitState::Ready;
                Step::Ready
            }
            InitState::Ready => Step::Ready,
            InitState::Failed => Step::Failed(Error::HardwareTimeout {
                operation: "initialization previously failed",
            }),
        }
    }
}

enum Step {
    Continue,
    Pending(OwnerInitSchedule),
    Ready,
    Failed(Error),
}

fn deadline(now_ns: u64, duration_ns: u64) -> u64 {
    now_ns.saturating_add(duration_ns)
}

fn wait_again(now_ns: u64, deadline_ns: u64) -> OwnerInitSchedule {
    OwnerInitSchedule::wait_until(deadline(now_ns, INIT_POLL_INTERVAL_NS).min(deadline_ns))
}

fn pending_or_timeout(now_ns: u64, deadline_ns: u64, error: Error) -> Step {
    if now_ns >= deadline_ns {
        Step::Failed(error)
    } else {
        Step::Pending(wait_again(now_ns, deadline_ns))
    }
}

fn valid_unicast_mac(mac: [u8; 6]) -> bool {
    mac != [0; 6] && mac != [u8::MAX; 6] && mac[0] & 1 == 0
}

fn chip_version_from_xid(xid: u16) -> ChipVersion {
    if xid & 0x07cf == 0x0641 {
        ChipVersion::Rtl8125B
    } else if xid & 0x07cf == 0x0609 {
        ChipVersion::Rtl8125A
    } else {
        ChipVersion::Unknown(xid)
    }
}

#[derive(Clone, Copy)]
struct EphyInfo {
    offset: u8,
    mask: u16,
    bits: u16,
}

fn ephy_program(chip: ChipVersion) -> &'static [EphyInfo] {
    match chip {
        ChipVersion::Rtl8125A => &RTL8125A_EPHY,
        ChipVersion::Rtl8125B | ChipVersion::Unknown(_) => &RTL8125B_EPHY,
    }
}

const RTL8125A_EPHY: [EphyInfo; 12] = [
    EphyInfo {
        offset: 0x04,
        mask: 0xffff,
        bits: 0xd000,
    },
    EphyInfo {
        offset: 0x0a,
        mask: 0xffff,
        bits: 0x8653,
    },
    EphyInfo {
        offset: 0x23,
        mask: 0xffff,
        bits: 0xab66,
    },
    EphyInfo {
        offset: 0x20,
        mask: 0xffff,
        bits: 0x9455,
    },
    EphyInfo {
        offset: 0x21,
        mask: 0xffff,
        bits: 0x99ff,
    },
    EphyInfo {
        offset: 0x29,
        mask: 0xffff,
        bits: 0xfe04,
    },
    EphyInfo {
        offset: 0x44,
        mask: 0xffff,
        bits: 0xd000,
    },
    EphyInfo {
        offset: 0x4a,
        mask: 0xffff,
        bits: 0x8653,
    },
    EphyInfo {
        offset: 0x63,
        mask: 0xffff,
        bits: 0xab66,
    },
    EphyInfo {
        offset: 0x60,
        mask: 0xffff,
        bits: 0x9455,
    },
    EphyInfo {
        offset: 0x61,
        mask: 0xffff,
        bits: 0x99ff,
    },
    EphyInfo {
        offset: 0x69,
        mask: 0xffff,
        bits: 0xfe04,
    },
];

const RTL8125B_EPHY: [EphyInfo; 6] = [
    EphyInfo {
        offset: 0x0b,
        mask: 0xffff,
        bits: 0xa908,
    },
    EphyInfo {
        offset: 0x1e,
        mask: 0xffff,
        bits: 0x20eb,
    },
    EphyInfo {
        offset: 0x4b,
        mask: 0xffff,
        bits: 0xa908,
    },
    EphyInfo {
        offset: 0x5e,
        mask: 0xffff,
        bits: 0x20eb,
    },
    EphyInfo {
        offset: 0x22,
        mask: 0x0030,
        bits: 0x0020,
    },
    EphyInfo {
        offset: 0x62,
        mask: 0x0030,
        bits: 0x0020,
    },
];

#[derive(Clone, Copy)]
enum DirectOp {
    ClearReadyToL23,
    ClearSpeedDown,
    Write8 { offset: usize, value: u8 },
    Write16 { offset: usize, value: u16 },
    MacWrite { register: u32, value: u16 },
    MacModify { register: u32, mask: u16, set: u16 },
}

fn apply_direct(regs: &Rtl8125OwnerInitRegs, operation: DirectOp, _chip: ChipVersion) {
    match operation {
        DirectOp::ClearReadyToL23 => regs.clear_ready_to_l23(),
        DirectOp::ClearSpeedDown => regs.clear_speed_down(),
        DirectOp::Write8 { offset, value } => regs.write_vendor_u8(offset, value),
        DirectOp::Write16 { offset, value } => regs.write_vendor_u16(offset, value),
        DirectOp::MacWrite { register, value } => regs.mac_ocp_write(register, value),
        DirectOp::MacModify {
            register,
            mask,
            set,
        } => regs.mac_ocp_modify(register, mask, set),
    }
}

const COMMON_PREFIX: [DirectOp; 10] = [
    DirectOp::ClearReadyToL23,
    DirectOp::Write16 {
        offset: 0x0382,
        value: 0x221b,
    },
    DirectOp::Write8 {
        offset: 0x4500,
        value: 0,
    },
    DirectOp::Write16 {
        offset: 0x4800,
        value: 0,
    },
    DirectOp::MacModify {
        register: 0xd40a,
        mask: 0x0010,
        set: 0,
    },
    DirectOp::ClearSpeedDown,
    DirectOp::MacWrite {
        register: 0xc140,
        value: 0xffff,
    },
    DirectOp::MacWrite {
        register: 0xc142,
        value: 0xffff,
    },
    DirectOp::MacModify {
        register: 0xd3e2,
        mask: 0x0fff,
        set: 0x03a9,
    },
    DirectOp::MacModify {
        register: 0xd3e4,
        mask: 0x00ff,
        set: 0,
    },
];

#[derive(Clone, Copy)]
struct CsiOp {
    address: u32,
    mask: u32,
    set: u32,
}

const CSI_PROGRAM: [CsiOp; 2] = [
    CsiOp {
        address: CSI_PCIE_CTRL,
        mask: CSI_PCIE_ZRXDC_NONCOMPL,
        set: 0,
    },
    CsiOp {
        address: CSI_L1_ENTRY_LATENCY & !0x3,
        mask: 0xff << ((CSI_L1_ENTRY_LATENCY & 0x3) * 8),
        set: (CSI_L1_ENTRY_LATENCY_DEFAULT as u32) << ((CSI_L1_ENTRY_LATENCY & 0x3) * 8),
    },
];

fn common_suffix(chip: ChipVersion) -> &'static [DirectOp] {
    match chip {
        ChipVersion::Rtl8125B => &COMMON_SUFFIX_B,
        ChipVersion::Rtl8125A | ChipVersion::Unknown(_) => &COMMON_SUFFIX_A,
    }
}

const COMMON_SUFFIX_A: [DirectOp; 14] = common_suffix_for(false);
const COMMON_SUFFIX_B: [DirectOp; 14] = common_suffix_for(true);

const fn common_suffix_for(chip_b: bool) -> [DirectOp; 14] {
    [
        DirectOp::MacModify {
            register: 0xe860,
            mask: 0,
            set: 0x0080,
        },
        DirectOp::MacModify {
            register: 0xeb58,
            mask: 0x0001,
            set: 0,
        },
        DirectOp::MacModify {
            register: 0xe614,
            mask: 0x0700,
            set: if chip_b { 0x0200 } else { 0x0400 },
        },
        DirectOp::MacModify {
            register: 0xe63e,
            mask: 0x0c30,
            set: if chip_b { 0 } else { 0x0020 },
        },
        DirectOp::MacModify {
            register: 0xc0b4,
            mask: 0,
            set: 0x000c,
        },
        DirectOp::MacModify {
            register: 0xeb6a,
            mask: 0x00ff,
            set: 0x0033,
        },
        DirectOp::MacModify {
            register: 0xeb50,
            mask: 0x03e0,
            set: 0x0040,
        },
        DirectOp::MacModify {
            register: 0xe056,
            mask: 0x00f0,
            set: 0x0030,
        },
        DirectOp::MacModify {
            register: 0xe040,
            mask: 0x1000,
            set: 0,
        },
        DirectOp::MacModify {
            register: 0xea1c,
            mask: 0x0003,
            set: 0x0001,
        },
        DirectOp::MacModify {
            register: 0xe0c0,
            mask: 0x4f0f,
            set: 0x4403,
        },
        DirectOp::MacModify {
            register: 0xe052,
            mask: 0x0080,
            set: 0x0068,
        },
        DirectOp::MacModify {
            register: 0xd430,
            mask: 0x0fff,
            set: 0x047f,
        },
        DirectOp::MacModify {
            register: 0xea1c,
            mask: 0x0004,
            set: 0,
        },
    ]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PhyOp {
    Write {
        page: u16,
        register: u8,
        value: u16,
    },
    Modify {
        page: u16,
        register: u8,
        mask: u16,
        set: u16,
    },
}

impl PhyOp {
    const fn page(self) -> u16 {
        match self {
            Self::Write { page, .. } | Self::Modify { page, .. } => page,
        }
    }

    const fn register(self) -> u8 {
        match self {
            Self::Write { register, .. } | Self::Modify { register, .. } => register,
        }
    }
}

const fn phy_write(page: u16, register: u8, value: u16) -> PhyOp {
    PhyOp::Write {
        page,
        register,
        value,
    }
}

const fn phy_modify(page: u16, register: u8, mask: u16, set: u16) -> PhyOp {
    PhyOp::Modify {
        page,
        register,
        mask,
        set,
    }
}

fn phy_program(chip: ChipVersion) -> &'static [PhyOp] {
    match chip {
        ChipVersion::Rtl8125A => PHY_PROGRAM_A,
        ChipVersion::Rtl8125B | ChipVersion::Unknown(_) => PHY_PROGRAM_B,
    }
}

const PHY_PROGRAM_A: &[PhyOp] = &[
    phy_modify(0x0ad4, 0x17, 0, 0x0010),
    phy_modify(0x0ad1, 0x13, 0x03ff, 0x03ff),
    phy_modify(0x0ad3, 0x11, 0x003f, 0x0006),
    phy_modify(0x0ac0, 0x14, 0x1100, 0),
    phy_modify(0x0acc, 0x10, 0x0003, 0x0002),
    phy_modify(0x0ad4, 0x10, 0x00e7, 0x0044),
    phy_modify(0x0ac1, 0x12, 0x0080, 0),
    phy_modify(0x0ac8, 0x10, 0x0300, 0),
    phy_modify(0x0ac5, 0x17, 0x0007, 0x0002),
    phy_write(0x0ad4, 0x16, 0x00a8),
    phy_write(0x0ac5, 0x16, 0x01ff),
    phy_modify(0x0ac8, 0x15, 0x00f0, 0x0030),
    phy_write(0x0b87, 0x16, 0x80a2),
    phy_write(0x0b87, 0x17, 0x0153),
    phy_write(0x0b87, 0x16, 0x809c),
    phy_write(0x0b87, 0x17, 0x0153),
    phy_write(0x0a43, 0x13, 0x8257),
    phy_modify(0x0a43, 0x14, 0xffff, 0x020f),
    phy_write(0x0a43, 0x13, 0x80ea),
    phy_modify(0x0a43, 0x14, 0xffff, 0x7843),
    phy_modify(0x0d06, 0x14, 0, 0x2000),
    phy_write(0x0a43, 0x13, 0x81a2),
    phy_modify(0x0a43, 0x14, 0, 0x0100),
    phy_modify(0x0b54, 0x16, 0xff00, 0xdb00),
    phy_modify(0x0a45, 0x12, 0x0001, 0),
    phy_modify(0x0a5d, 0x12, 0, 0x0020),
    phy_modify(0x0ad4, 0x17, 0x0010, 0),
    phy_modify(0x0a86, 0x15, 0x0001, 0),
    phy_modify(0x0a44, 0x11, 0, 1 << 11),
    phy_modify(0x0a43, 0x11, 0, 1 << 4),
    phy_modify(0x0a4a, 0x11, 0, 1 << 9),
    phy_modify(0x0a42, 0x14, 0, 1 << 7),
    phy_modify(0x0a6d, 0x12, 1, 0),
    phy_modify(0x0a6d, 0x14, 1 << 4, 0),
    phy_modify(0, MII_CTRL1000, ADVERTISE_1000_MASK, ADVERTISE_1000FULL),
    phy_modify(
        0,
        MII_BMCR,
        BMCR_AUTONEG_MASK,
        BMCR_ANENABLE | BMCR_ANRESTART,
    ),
];

const PHY_PROGRAM_B: &[PhyOp] = &[
    phy_modify(0x0a44, 0x11, 0, 0x0800),
    phy_modify(0x0ac4, 0x13, 0x00f0, 0x0090),
    phy_modify(0x0ad3, 0x10, 0x0003, 0x0001),
    phy_write(0x0b87, 0x16, 0x80f5),
    phy_write(0x0b87, 0x17, 0x760e),
    phy_write(0x0b87, 0x16, 0x8107),
    phy_write(0x0b87, 0x17, 0x360e),
    phy_write(0x0b87, 0x16, 0x8551),
    phy_modify(0x0b87, 0x17, 0xff00, 0x0800),
    phy_modify(0x0bf0, 0x10, 0xe000, 0xa000),
    phy_modify(0x0bf4, 0x13, 0x0f00, 0x0300),
    phy_write(0x0a43, 0x13, 0x8044),
    phy_modify(0x0a43, 0x14, 0xffff, 0x2417),
    phy_write(0x0a43, 0x13, 0x804a),
    phy_modify(0x0a43, 0x14, 0xffff, 0x2417),
    phy_write(0x0a43, 0x13, 0x8050),
    phy_modify(0x0a43, 0x14, 0xffff, 0x2417),
    phy_write(0x0a43, 0x13, 0x8056),
    phy_modify(0x0a43, 0x14, 0xffff, 0x2417),
    phy_write(0x0a43, 0x13, 0x805c),
    phy_modify(0x0a43, 0x14, 0xffff, 0x2417),
    phy_write(0x0a43, 0x13, 0x8062),
    phy_modify(0x0a43, 0x14, 0xffff, 0x2417),
    phy_write(0x0a43, 0x13, 0x8068),
    phy_modify(0x0a43, 0x14, 0xffff, 0x2417),
    phy_write(0x0a43, 0x13, 0x806e),
    phy_modify(0x0a43, 0x14, 0xffff, 0x2417),
    phy_write(0x0a43, 0x13, 0x8074),
    phy_modify(0x0a43, 0x14, 0xffff, 0x2417),
    phy_write(0x0a43, 0x13, 0x807a),
    phy_modify(0x0a43, 0x14, 0xffff, 0x2417),
    phy_modify(0x0a4c, 0x15, 0, 0x0040),
    phy_modify(0x0bf8, 0x12, 0xe000, 0xa000),
    phy_modify(0x0a5b, 0x12, 1 << 15, 0),
    phy_modify(0x0a6d, 0x12, 1, 0),
    phy_modify(0x0a6d, 0x14, 1 << 4, 0),
    phy_modify(0x0a42, 0x14, 1 << 7, 0),
    phy_modify(0x0a4a, 0x11, 1 << 9, 0),
    phy_modify(0, MII_CTRL1000, ADVERTISE_1000_MASK, ADVERTISE_1000FULL),
    phy_modify(
        0,
        MII_BMCR,
        BMCR_AUTONEG_MASK,
        BMCR_ANENABLE | BMCR_ANRESTART,
    ),
];

fn phy_address(page: u16, register: u8) -> u32 {
    if page == 0 {
        OCP_STD_PHY_BASE + u32::from(register) * 2
    } else {
        (u32::from(page) << 4) + u32::from(register.saturating_sub(0x10)) * 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phy_program_advertises_gigabit_before_restarting_autoneg() {
        for program in [PHY_PROGRAM_A, PHY_PROGRAM_B] {
            let advertise = program
                .iter()
                .position(|op| {
                    matches!(
                        op,
                        PhyOp::Modify {
                            page: 0,
                            register: MII_CTRL1000,
                            ..
                        }
                    )
                })
                .expect("gigabit advertisement operation");
            let restart = program
                .iter()
                .position(|op| {
                    matches!(
                        op,
                        PhyOp::Modify {
                            page: 0,
                            register: MII_BMCR,
                            ..
                        }
                    )
                })
                .expect("autoneg restart operation");
            assert!(advertise < restart);
        }
    }
}

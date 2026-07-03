use sdmmc_protocol::{error::Error, sdio::host::ClockSpeed};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Sd,
    Mmc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimingTable {
    pub target_hz: u32,
    pub use_hold: bool,
    pub clk_div: u32,
    pub clk_src: u32,
    pub shift: u32,
}

impl TimingTable {
    pub fn sd_for_speed(speed: ClockSpeed) -> Result<Self, Error> {
        Self::for_speed(speed, MediaKind::Sd)
    }

    pub fn for_speed(speed: ClockSpeed, media: MediaKind) -> Result<Self, Error> {
        match (speed, media) {
            (ClockSpeed::Identification, _) => Ok(MMC_SD_400KHZ),
            (ClockSpeed::Default | ClockSpeed::Sdr12, MediaKind::Sd) => Ok(SD_25MHZ),
            (ClockSpeed::HighSpeed | ClockSpeed::Sdr25, MediaKind::Sd) => Ok(SD_50MHZ),
            (ClockSpeed::Sdr50 | ClockSpeed::Ddr50, MediaKind::Sd) => Ok(SD_100MHZ),
            (ClockSpeed::Default | ClockSpeed::Sdr12, MediaKind::Mmc) => Ok(MMC_26MHZ),
            (ClockSpeed::HighSpeed | ClockSpeed::Sdr25, MediaKind::Mmc) => Ok(MMC_52MHZ),
            (ClockSpeed::Sdr50 | ClockSpeed::Ddr50, MediaKind::Mmc) => Ok(MMC_100MHZ),
            (ClockSpeed::Hs200, MediaKind::Mmc) => Ok(MMC_100MHZ),
            (ClockSpeed::Sdr104 | ClockSpeed::Hs200, MediaKind::Sd)
            | (ClockSpeed::Sdr104, MediaKind::Mmc) => Err(Error::UnsupportedCommand),
            // Future ClockSpeed variants are not represented in this timing table.
            (..) => Err(Error::UnsupportedCommand),
        }
    }
}

pub const MMC_SD_400KHZ: TimingTable = TimingTable {
    target_hz: 400_000,
    use_hold: true,
    clk_div: 0x7e7dfa,
    clk_src: 0x000502,
    shift: 0,
};

pub const SD_25MHZ: TimingTable = TimingTable {
    target_hz: 25_000_000,
    use_hold: true,
    clk_div: 0x030204,
    clk_src: 0x000302,
    shift: 0,
};

pub const SD_50MHZ: TimingTable = TimingTable {
    target_hz: 50_000_000,
    use_hold: true,
    clk_div: 0x030204,
    clk_src: 0x000502,
    shift: 0,
};

pub const SD_100MHZ: TimingTable = TimingTable {
    target_hz: 100_000_000,
    use_hold: false,
    clk_div: 0x010002,
    clk_src: 0x000202,
    shift: 0,
};

pub const MMC_26MHZ: TimingTable = TimingTable {
    target_hz: 26_000_000,
    use_hold: true,
    clk_div: 0x030204,
    clk_src: 0x000302,
    shift: 0,
};

pub const MMC_52MHZ: TimingTable = TimingTable {
    target_hz: 52_000_000,
    use_hold: false,
    clk_div: 0x030204,
    clk_src: 0x000202,
    shift: 0,
};

pub const MMC_100MHZ: TimingTable = TimingTable {
    target_hz: 100_000_000,
    use_hold: false,
    clk_div: 0x010002,
    clk_src: 0x000202,
    shift: 0,
};

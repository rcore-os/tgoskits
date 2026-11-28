#![allow(dead_code)]

use core::ops::{Deref, DerefMut};

use ax_runtime::serial::{DataBits, Framing, Parity, StopBits};
use bytemuck::AnyBitPattern;
use linux_raw_sys::general::{
    B0, B50, B75, B110, B134, B150, B200, B300, B600, B1200, B1800, B2400, B4800, B9600, B19200,
    B38400, B57600, B115200, B230400, B460800, B500000, B576000, B921600, B1000000, B1152000,
    B1500000, B2000000, B2500000, B3000000, B3500000, B4000000, BOTHER, CBAUD, CMSPAR, CREAD, CS5,
    CS6, CS7, CS8, CSIZE, CSTOPB, ECHO, ECHOCTL, ECHOE, ECHOK, ECHOKE, IBSHIFT, ICANON, ICRNL,
    IEXTEN, ISIG, IXON, ONLCR, OPOST, PARENB, PARODD, VDISCARD, VEOF, VEOL, VEOL2, VERASE, VINTR,
    VKILL, VLNEXT, VQUIT, VREPRINT, VSUSP, VWERASE, speed_t, tcflag_t,
};
use starry_signal::Signo;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TermiosParity {
    None,
    Odd,
    Even,
    Mark,
    Space,
}

#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern, bytemuck::NoUninit)]
pub struct Termios {
    c_iflag: tcflag_t,
    c_oflag: tcflag_t,
    c_cflag: tcflag_t,
    c_lflag: tcflag_t,
    c_line: u8,
    c_cc: [u8; 19usize],
}

impl Default for Termios {
    fn default() -> Self {
        let mut result = Self {
            c_iflag: ICRNL | IXON,
            c_oflag: OPOST | ONLCR,
            c_cflag: B38400 | CS8 | CREAD,
            c_lflag: ICANON | ECHO | ISIG | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN,
            c_line: 0,
            c_cc: [0; 19],
        };

        fn ctl(ch: u8) -> u8 {
            ch - 0x40
        }
        for (i, ch) in [
            (VINTR, ctl(b'C')),
            (VQUIT, ctl(b'\\')),
            (VERASE, b'\x7f'),
            (VKILL, ctl(b'U')),
            (VEOF, ctl(b'D')),
            (VEOL, b'\0'),
            (VREPRINT, ctl(b'R')),
            (VDISCARD, ctl(b'O')),
            (VWERASE, ctl(b'W')),
            (VLNEXT, ctl(b'V')),
            (VEOL2, b'\0'),
            (VSUSP, ctl(b'Z')),
        ] {
            result.c_cc[i as usize] = ch;
        }

        result
    }
}

impl Termios {
    pub fn special_char(&self, index: u32) -> u8 {
        self.c_cc[index as usize]
    }

    pub fn has_iflag(&self, flag: u32) -> bool {
        self.c_iflag & flag != 0
    }

    pub fn has_oflag(&self, flag: u32) -> bool {
        self.c_oflag & flag != 0
    }

    pub fn has_cflag(&self, flag: u32) -> bool {
        self.c_cflag & flag != 0
    }

    pub fn cflag(&self) -> tcflag_t {
        self.c_cflag
    }

    pub fn data_bits(&self) -> u8 {
        match self.c_cflag & CSIZE {
            CS5 => 5,
            CS6 => 6,
            CS7 => 7,
            _ => 8,
        }
    }

    pub fn stop_bits(&self) -> u8 {
        if self.has_cflag(CSTOPB) { 2 } else { 1 }
    }

    pub fn parity(&self) -> TermiosParity {
        match self.framing().parity {
            Parity::None => TermiosParity::None,
            Parity::Odd => TermiosParity::Odd,
            Parity::Even => TermiosParity::Even,
            Parity::Mark => TermiosParity::Mark,
            Parity::Space => TermiosParity::Space,
        }
    }

    pub fn framing(&self) -> Framing {
        let data_bits = match self.c_cflag & CSIZE {
            CS5 => DataBits::Five,
            CS6 => DataBits::Six,
            CS7 => DataBits::Seven,
            _ => DataBits::Eight,
        };
        let stop_bits = if self.has_cflag(CSTOPB) {
            StopBits::Two
        } else {
            StopBits::One
        };
        let parity = match self.c_cflag & (PARENB | PARODD | CMSPAR) {
            flags if flags & PARENB == 0 => Parity::None,
            PARENB => Parity::Even,
            flags if flags == PARENB | PARODD => Parity::Odd,
            flags if flags == PARENB | CMSPAR => Parity::Space,
            _ => Parity::Mark,
        };
        Framing {
            data_bits,
            stop_bits,
            parity,
        }
    }

    pub fn has_lflag(&self, flag: u32) -> bool {
        self.c_lflag & flag != 0
    }

    pub fn echo(&self) -> bool {
        self.has_lflag(ECHO)
    }

    pub fn canonical(&self) -> bool {
        self.has_lflag(ICANON)
    }

    pub fn contains_iexten(&self) -> bool {
        self.has_lflag(IEXTEN)
    }

    pub fn is_eol(&self, ch: u8) -> bool {
        if ch == b'\n' || ch == self.special_char(VEOL) {
            return true;
        }

        if self.contains_iexten() && ch == self.special_char(VEOL2) {
            return true;
        }

        false
    }

    pub fn signo_for(&self, ch: u8) -> Option<Signo> {
        Some(match ch {
            ch if ch == self.special_char(VINTR) => Signo::SIGINT,
            ch if ch == self.special_char(VQUIT) => Signo::SIGQUIT,
            ch if ch == self.special_char(VSUSP) => Signo::SIGTSTP,
            _ => return None,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern, bytemuck::NoUninit)]
pub struct Termios2 {
    termios: Termios,
    c_ispeed: speed_t,
    c_ospeed: speed_t,
}

impl Default for Termios2 {
    fn default() -> Self {
        Self {
            termios: Termios::default(),
            c_ispeed: 38_400,
            c_ospeed: 38_400,
        }
    }
}

impl Termios2 {
    pub fn new(termios: Termios) -> Self {
        Self {
            termios,
            c_ispeed: B38400,
            c_ospeed: B38400,
        }
    }

    pub fn with_legacy(self, termios: Termios) -> Self {
        Self { termios, ..self }
    }

    pub fn input_speed(&self) -> speed_t {
        self.c_ispeed
    }

    pub fn output_speed(&self) -> speed_t {
        self.c_ospeed
    }

    pub fn default_b115200() -> Self {
        Self::with_baudrate(115_200)
    }

    pub fn with_baudrate(baudrate: u32) -> Self {
        let mut termios = Self::default();
        termios.set_baudrate(baudrate);
        termios
    }

    pub fn set_baudrate(&mut self, baudrate: u32) {
        let encoding = BAUDRATES
            .iter()
            .find_map(|&(encoding, rate)| (rate == baudrate).then_some(encoding))
            .unwrap_or(BOTHER);
        self.c_cflag = (self.c_cflag & !CBAUD) | encoding;
        self.c_ispeed = baudrate;
        self.c_ospeed = baudrate;
    }

    pub fn with_unknown_baudrate() -> Self {
        // Report an unreadable firmware divisor as a zero numeric speed
        // without advertising the B0 modem-output transition.
        let mut termios = Self::with_baudrate(0);
        termios.c_cflag |= BOTHER;
        termios
    }

    pub fn use_output_baudrate_for_input(&mut self) {
        let output = self.c_cflag & CBAUD;
        let input_mask = CBAUD << IBSHIFT;
        if output != B0 {
            self.c_ispeed = self.c_ospeed;
            if self.c_cflag & input_mask != 0 {
                self.c_cflag = (self.c_cflag & !input_mask) | (output << IBSHIFT);
            }
        }
    }

    pub fn set_framing(&mut self, framing: Framing) {
        let size = match framing.data_bits {
            DataBits::Five => CS5,
            DataBits::Six => CS6,
            DataBits::Seven => CS7,
            DataBits::Eight => CS8,
        };
        let stop = match framing.stop_bits {
            StopBits::One => 0,
            StopBits::Two => CSTOPB,
        };
        let parity = match framing.parity {
            Parity::None => 0,
            Parity::Odd => PARENB | PARODD,
            Parity::Even => PARENB,
            Parity::Mark => PARENB | PARODD | CMSPAR,
            Parity::Space => PARENB | CMSPAR,
        };
        self.c_cflag =
            (self.c_cflag & !(CSIZE | CSTOPB | PARENB | PARODD | CMSPAR)) | size | stop | parity;
    }

    pub fn normalize_baudrates(&mut self) {
        self.c_ospeed = self.baudrate().unwrap_or(0);
        self.c_ispeed = match (self.cflag() >> IBSHIFT) & CBAUD {
            B0 => self.c_ospeed,
            BOTHER => self.c_ispeed,
            encoding => baudrate_from_constant(encoding),
        };
    }

    pub fn is_hangup(&self) -> bool {
        self.cflag() & CBAUD == B0
    }

    pub fn baudrate(&self) -> Option<u32> {
        match self.cflag() & CBAUD {
            B0 => None,
            BOTHER => (self.c_ospeed != 0).then_some(self.c_ospeed),
            encoding => Some(baudrate_from_constant(encoding)),
        }
    }
}

const BAUDRATES: [(tcflag_t, speed_t); 31] = [
    (B0, 0),
    (B50, 50),
    (B75, 75),
    (B110, 110),
    (B134, 134),
    (B150, 150),
    (B200, 200),
    (B300, 300),
    (B600, 600),
    (B1200, 1_200),
    (B1800, 1_800),
    (B2400, 2_400),
    (B4800, 4_800),
    (B9600, 9_600),
    (B19200, 19_200),
    (B38400, 38_400),
    (B57600, 57_600),
    (B115200, 115_200),
    (B230400, 230_400),
    (B460800, 460_800),
    (B500000, 500_000),
    (B576000, 576_000),
    (B921600, 921_600),
    (B1000000, 1_000_000),
    (B1152000, 1_152_000),
    (B1500000, 1_500_000),
    (B2000000, 2_000_000),
    (B2500000, 2_500_000),
    (B3000000, 3_000_000),
    (B3500000, 3_500_000),
    (B4000000, 4_000_000),
];

fn baudrate_from_constant(encoding: tcflag_t) -> speed_t {
    BAUDRATES
        .iter()
        .find_map(|&(flag, rate)| (flag == encoding).then_some(rate))
        .unwrap_or(0)
}

impl Deref for Termios2 {
    type Target = Termios;

    fn deref(&self) -> &Self::Target {
        &self.termios
    }
}

impl DerefMut for Termios2 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.termios
    }
}

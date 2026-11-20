#![allow(dead_code)]

use core::ops::{Deref, DerefMut};

use bytemuck::AnyBitPattern;
use linux_raw_sys::general::{
    B0, B50, B75, B110, B134, B150, B200, B300, B600, B1200, B1800, B2400, B4800, B9600, B19200,
    B38400, B57600, B115200, B230400, B460800, B500000, B576000, B921600, B1000000, B1152000,
    B1500000, B2000000, B2500000, B3000000, B3500000, B4000000, BOTHER, CBAUD, CIBAUD, CMSPAR,
    CREAD, CS5, CS6, CS7, CS8, CSIZE, CSTOPB, ECHO, ECHOCTL, ECHOE, ECHOK, ECHOKE, IBSHIFT, ICANON,
    ICRNL, IEXTEN, ISIG, IXON, ONLCR, OPOST, PARENB, PARODD, VDISCARD, VEOF, VEOL, VEOL2, VERASE,
    VINTR, VKILL, VLNEXT, VQUIT, VREPRINT, VSUSP, VWERASE, speed_t, tcflag_t,
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
            CS8 => 8,
            _ => 8,
        }
    }

    pub fn stop_bits(&self) -> u8 {
        if self.has_cflag(CSTOPB) { 2 } else { 1 }
    }

    pub fn parity(&self) -> TermiosParity {
        if !self.has_cflag(PARENB) {
            return TermiosParity::None;
        }
        if self.has_cflag(CMSPAR) {
            if self.has_cflag(PARODD) {
                TermiosParity::Mark
            } else {
                TermiosParity::Space
            }
        } else if self.has_cflag(PARODD) {
            TermiosParity::Odd
        } else {
            TermiosParity::Even
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
        Self::new(Termios::default())
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

    pub fn default_b115200() -> Self {
        let mut termios = Termios::default();
        termios.c_cflag = (termios.c_cflag & !CBAUD) | B115200;
        Self {
            termios,
            c_ispeed: B115200,
            c_ospeed: B115200,
        }
    }

    pub(super) fn with_inherited_baudrate(baudrate: u32) -> Self {
        let speed = BAUD_RATES
            .iter()
            .find_map(|&(speed, rate)| (rate == baudrate).then_some(speed))
            .unwrap_or(BOTHER);
        let mut termios = Termios::default();
        termios.c_cflag = (termios.c_cflag & !CBAUD) | speed;
        Self {
            termios,
            c_ispeed: baudrate,
            c_ospeed: baudrate,
        }
    }

    pub(super) fn retain_inherited_baudrate(&mut self, baudrate: u32, legacy: bool) -> bool {
        let output = self.cflag() & CBAUD;
        let input = (self.cflag() & CIBAUD) >> IBSHIFT;
        let matches_rate = |speed, numeric| {
            if speed == BOTHER {
                // Legacy TCSETS leaves the saved numeric rate in effect.
                legacy || numeric == baudrate
            } else {
                baudrate_from_constant(speed) == Some(baudrate)
            }
        };
        let hangup = self.requests_hangup();
        if (!hangup && !matches_rate(output, self.c_ospeed))
            || (input != B0 && !matches_rate(input, self.c_ispeed))
        {
            return false;
        }
        self.c_ospeed = if hangup { 0 } else { baudrate };
        self.c_ispeed = if input == B0 { self.c_ospeed } else { baudrate };
        true
    }

    pub(in crate::pseudofs::dev::tty) fn requests_hangup(&self) -> bool {
        self.cflag() & CBAUD == B0
    }

    pub fn input_speed(&self) -> speed_t {
        self.c_ispeed
    }

    pub fn output_speed(&self) -> speed_t {
        self.c_ospeed
    }

    pub fn baudrate(&self) -> Option<u32> {
        let speed = if self.output_speed() != 0 {
            self.output_speed()
        } else {
            self.input_speed()
        };
        if speed != 0 && self.cflag() & CBAUD == BOTHER {
            return Some(speed);
        }
        baudrate_from_constant(self.cflag() & CBAUD)
    }
}

const BAUD_RATES: &[(speed_t, u32)] = &[
    (B50, 50),
    (B75, 75),
    (B110, 110),
    (B134, 134),
    (B150, 150),
    (B200, 200),
    (B300, 300),
    (B600, 600),
    (B1200, 1200),
    (B1800, 1800),
    (B2400, 2400),
    (B4800, 4800),
    (B9600, 9600),
    (B19200, 19200),
    (B38400, 38400),
    (B57600, 57600),
    (B115200, 115200),
    (B230400, 230400),
    (B460800, 460800),
    (B500000, 500000),
    (B576000, 576000),
    (B921600, 921600),
    (B1000000, 1000000),
    (B1152000, 1152000),
    (B1500000, 1500000),
    (B2000000, 2000000),
    (B2500000, 2500000),
    (B3000000, 3000000),
    (B3500000, 3500000),
    (B4000000, 4000000),
];

fn baudrate_from_constant(speed: speed_t) -> Option<u32> {
    BAUD_RATES
        .iter()
        .find_map(|&(constant, rate)| (constant == speed).then_some(rate))
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

#[cfg(all(test, axtest))]
#[axtest::axtest]
fn inherited_serial_termios_preserves_firmware_speed() {
    for (baudrate, code) in [(115_200, B115200), (123_456, BOTHER), (0, BOTHER)] {
        let terminal = super::Terminal::new(Some(baudrate));
        let initial = terminal.load_termios();
        assert_eq!(initial.cflag() & CBAUD, code);
        assert_eq!(initial.input_speed(), baudrate);
        assert_eq!(initial.output_speed(), baudrate);

        let mut changed = *initial;
        changed.c_cflag |= CSTOPB;
        let updated = terminal.prepare_termios(changed, false).unwrap();
        assert_eq!(updated.stop_bits(), 2);
        assert_eq!(updated.input_speed(), baudrate);
        assert_eq!(updated.output_speed(), baudrate);

        for flags in [
            changed.cflag(),
            (changed.cflag() & !CBAUD) | BOTHER,
            (changed.cflag() & !CIBAUD) | (BOTHER << IBSHIFT),
        ] {
            let mut legacy = changed.termios;
            legacy.c_cflag = flags;
            let updated = terminal
                .prepare_termios(Termios2::new(legacy), true)
                .unwrap();
            assert_eq!(updated.cflag(), flags);
            assert_eq!(updated.stop_bits(), 2);
            assert_eq!(updated.input_speed(), baudrate);
            assert_eq!(updated.output_speed(), baudrate);
        }

        for flags in [
            (initial.cflag() & !CBAUD) | B9600,
            (initial.cflag() & !CIBAUD) | (B9600 << IBSHIFT),
        ] {
            changed.c_cflag = flags;
            assert!(matches!(
                terminal.prepare_termios(changed, false),
                Err(crate::StarryError::InvalidInput)
            ));
        }

        assert!(!initial.requests_hangup());
        let mut hangup = *initial;
        hangup.c_cflag &= !CBAUD;
        let updated = terminal.prepare_termios(hangup, false).unwrap();
        assert!(updated.requests_hangup());
        assert_eq!(updated.input_speed(), 0);
        assert_eq!(updated.output_speed(), 0);
        hangup.c_cflag |= BOTHER << IBSHIFT;
        let updated = terminal.prepare_termios(hangup, false).unwrap();
        assert!(updated.requests_hangup());
        assert_eq!(updated.input_speed(), baudrate);
        assert_eq!(updated.output_speed(), 0);

        for (flags, input, output) in [
            ((initial.cflag() & !CBAUD) | BOTHER, baudrate, baudrate + 1),
            (
                (initial.cflag() & !CIBAUD) | (BOTHER << IBSHIFT),
                baudrate + 1,
                baudrate,
            ),
        ] {
            changed.c_cflag = flags;
            changed.c_ispeed = input;
            changed.c_ospeed = output;
            assert!(matches!(
                terminal.prepare_termios(changed, false),
                Err(crate::StarryError::InvalidInput)
            ));
        }
    }
}

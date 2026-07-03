#![allow(dead_code)]

use core::ops::{Deref, DerefMut};

use bytemuck::AnyBitPattern;
use linux_raw_sys::general::{
    B50, B75, B110, B134, B150, B200, B300, B600, B1200, B1800, B2400, B4800, B9600, B19200,
    B38400, B57600, B115200, B230400, B460800, B500000, B576000, B921600, B1000000, B1152000,
    B1500000, B2000000, B2500000, B3000000, B3500000, B4000000, BOTHER, CBAUD, CMSPAR, CREAD, CS5,
    CS6, CS7, CS8, CSIZE, CSTOPB, ECHO, ECHOCTL, ECHOE, ECHOK, ECHOKE, ICANON, ICRNL, IEXTEN, ISIG,
    IXON, ONLCR, OPOST, PARENB, PARODD, VDISCARD, VEOF, VEOL, VEOL2, VERASE, VINTR, VKILL, VLNEXT,
    VQUIT, VREPRINT, VSUSP, VWERASE, speed_t, tcflag_t,
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
#[derive(Clone, Copy, AnyBitPattern)]
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
#[derive(Clone, Copy, AnyBitPattern)]
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

fn baudrate_from_constant(speed: speed_t) -> Option<u32> {
    Some(match speed {
        B50 => 50,
        B75 => 75,
        B110 => 110,
        B134 => 134,
        B150 => 150,
        B200 => 200,
        B300 => 300,
        B600 => 600,
        B1200 => 1200,
        B1800 => 1800,
        B2400 => 2400,
        B4800 => 4800,
        B9600 => 9600,
        B19200 => 19200,
        B38400 => 38400,
        B57600 => 57600,
        B115200 => 115200,
        B230400 => 230400,
        B460800 => 460800,
        B500000 => 500000,
        B576000 => 576000,
        B921600 => 921600,
        B1000000 => 1000000,
        B1152000 => 1152000,
        B1500000 => 1500000,
        B2000000 => 2000000,
        B2500000 => 2500000,
        B3000000 => 3000000,
        B3500000 => 3500000,
        B4000000 => 4000000,
        _ => return None,
    })
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

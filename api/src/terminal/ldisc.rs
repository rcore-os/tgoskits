use alloc::{sync::Arc, vec::Vec};
use core::ops::Range;

use axerrno::{LinuxError, LinuxResult};
use linux_raw_sys::general::{
    ECHOCTL, ECHOK, ICRNL, IGNCR, ISIG, VEOF, VERASE, VKILL, VMIN, VTIME,
};
use starry_core::task::send_signal_to_process_group;
use starry_signal::SignalInfo;

use crate::terminal::{job::JobControl, termios::Termios2};

pub struct LineDiscipline {
    pub termios: Termios2,
    job_control: Arc<JobControl>,

    read_buf: [u8; 32],
    read_range: Range<usize>,

    line_buf: Vec<u8>,
    line_read: Option<usize>,
}

impl LineDiscipline {
    pub fn new(job_control: Arc<JobControl>) -> Self {
        Self {
            termios: Termios2::default(),
            job_control,
            read_buf: [0; 32],
            read_range: 0..0,
            line_buf: Vec::new(),
            line_read: None,
        }
    }

    pub fn read(&mut self, buf: &mut [u8]) -> LinuxResult<usize> {
        if self.termios.canonical() {
            loop {
                let read = self.poll_read(buf)?;
                if read > 0 {
                    return Ok(read);
                }
                axtask::yield_now();
            }
        }
        let vmin = self.termios.special_char(VMIN);
        let vtime = self.termios.special_char(VTIME);
        if vtime > 0 {
            todo!();
        }

        if buf.len() < vmin as usize {
            return Err(LinuxError::EAGAIN);
        }
        let mut total_read = 0;
        loop {
            let read = self.poll_read(&mut buf[total_read..])?;
            total_read += read;
            if total_read >= vmin as usize {
                return Ok(total_read);
            }
            axtask::yield_now();
        }
    }

    pub fn poll_read(&mut self, buf: &mut [u8]) -> LinuxResult<usize> {
        let term = &self.termios;
        let mut read = 0;
        while read < buf.len() {
            if let Some(start) = &mut self.line_read {
                let dest = &mut buf[read..];
                let len = dest.len().min(self.line_buf.len() - *start);
                dest[..len].copy_from_slice(&self.line_buf[*start..]);
                read += len;
                *start += len;
                if *start == self.line_buf.len() {
                    self.line_read = None;
                    self.line_buf.clear();
                }
                continue;
            }
            if self.read_range.is_empty() {
                let read = axhal::console::read_bytes(&mut self.read_buf);
                if read == 0 {
                    break;
                }
                self.read_range = 0..read;
            }

            let mut ch = self.read_buf[self.read_range.start];
            self.read_range.start += 1;

            if ch == b'\r' {
                if term.has_iflag(IGNCR) {
                    continue;
                }
                if term.has_iflag(ICRNL) {
                    ch = b'\n';
                }
            }

            self.check_send_signal(ch)?;

            if term.echo() {
                self.output_char(ch);
            }
            if !term.canonical() {
                buf[read] = ch;
                read += 1;
                continue;
            }

            // Canonical mode
            if term.has_lflag(ECHOK) && ch == term.special_char(VKILL) {
                self.line_buf.clear();
                continue;
            }
            if ch == term.special_char(VERASE) {
                self.line_buf.pop();
                continue;
            }

            if term.is_eol(ch) || ch == term.special_char(VEOF) {
                if ch != term.special_char(VEOF) {
                    self.line_buf.push(ch);
                }
                self.line_read = Some(0);
                continue;
            }

            if ch.is_ascii_graphic() {
                self.line_buf.push(ch);
                continue;
            }

            warn!("Ignored char: {:#x}", ch);
        }

        Ok(read)
    }

    fn check_send_signal(&self, ch: u8) -> LinuxResult<()> {
        if !self.termios.canonical() || !self.termios.has_lflag(ISIG) {
            return Ok(());
        }
        if let Some(signo) = self.termios.signo_for(ch)
            && let Some(pg) = self.job_control.foreground()
        {
            let sig = SignalInfo::new_kernel(signo);
            send_signal_to_process_group(pg.pgid(), Some(sig))?;
        }

        Ok(())
    }

    fn output_char(&self, ch: u8) {
        use axhal::console::write_bytes;
        match ch {
            b'\n' => write_bytes(b"\n"),
            b'\r' => write_bytes(b"\r\n"),
            ch if ch == self.termios.special_char(VERASE) => write_bytes(b"\x08 \x08"),
            ch if ch.is_ascii_graphic() => write_bytes(&[ch]),
            ch if ch.is_ascii_control() && self.termios.has_lflag(ECHOCTL) => {
                write_bytes(&[b'^', (ch + 0x40)]);
            }
            other => {
                warn!("Ignored char: {:#x}", other);
            }
        }
    }
}

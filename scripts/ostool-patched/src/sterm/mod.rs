//! Async terminal core shared by local serial, remote websocket, and process I/O.
//!
//! Press `Ctrl+A` followed by `x` to exit when the exit sequence is enabled.

use std::{
    io::{self, IsTerminal, Write},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::anyhow;
use crossterm::{
    event::{
        Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
        MouseEvent, MouseEventKind,
    },
    terminal::{disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use tokio::{
    sync::{mpsc, watch},
    time::{Instant, sleep_until},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalAction {
    SendBytes(Vec<u8>),
    ExitRequested,
    Noop,
}

#[derive(Debug, Clone)]
pub struct TerminalConfig {
    pub intercept_exit_sequence: bool,
    pub timeout: Option<Duration>,
    pub timeout_label: String,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            intercept_exit_sequence: true,
            timeout: None,
            timeout_label: "terminal".to_string(),
        }
    }
}

pub struct AsyncTerminal {
    config: TerminalConfig,
    key_processor: KeyProcessor,
}

#[derive(Clone)]
pub struct TerminalHandle {
    inner: Arc<TerminalState>,
}

struct TerminalState {
    running: AtomicBool,
    timed_out: AtomicBool,
    stop_deadline: Mutex<Option<Instant>>,
    timeout_deadline: Mutex<Option<Instant>>,
    outbound_tx: mpsc::UnboundedSender<Vec<u8>>,
    wake_version: AtomicU64,
    wake_tx: watch::Sender<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeySequenceState {
    Normal,
    CtrlAPressed,
}

#[derive(Debug, Clone)]
struct KeyProcessor {
    intercept_exit_sequence: bool,
    state: KeySequenceState,
}

impl AsyncTerminal {
    pub fn new(config: TerminalConfig) -> Self {
        let key_processor = KeyProcessor::new(config.intercept_exit_sequence);
        Self {
            config,
            key_processor,
        }
    }

    pub async fn run<F>(
        self,
        inbound_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        outbound_tx: mpsc::UnboundedSender<Vec<u8>>,
        on_byte: F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(&TerminalHandle, u8) + Send,
    {
        self.run_with_output(inbound_rx, outbound_tx, io::stdout(), on_byte)
            .await
    }

    async fn run_with_output<W, F>(
        self,
        mut inbound_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        outbound_tx: mpsc::UnboundedSender<Vec<u8>>,
        mut output: W,
        mut on_byte: F,
    ) -> anyhow::Result<()>
    where
        W: Write,
        F: FnMut(&TerminalHandle, u8) + Send,
    {
        let interactive_input_enabled = io::stdin().is_terminal() && io::stdout().is_terminal();
        self.run_with_output_mode(
            &mut inbound_rx,
            outbound_tx,
            &mut output,
            &mut on_byte,
            interactive_input_enabled,
        )
        .await
    }

    async fn run_with_output_mode<W, F>(
        mut self,
        inbound_rx: &mut mpsc::UnboundedReceiver<Vec<u8>>,
        outbound_tx: mpsc::UnboundedSender<Vec<u8>>,
        output: &mut W,
        on_byte: &mut F,
        interactive_input_enabled: bool,
    ) -> anyhow::Result<()>
    where
        W: Write,
        F: FnMut(&TerminalHandle, u8) + Send,
    {
        if interactive_input_enabled {
            enable_raw_mode().ok();
            // Forward mouse events to the guest so TUI apps (jcode, etc.) can
            // use scroll and click.  We enable SGR mouse mode (?1006h) on the
            // host terminal so coordinates are not limited to 223.
            crossterm::execute!(
                std::io::stdout(),
                crossterm::event::EnableMouseCapture
            )
            .ok();
        } else {
            debug!("keyboard input disabled because stdin/stdout are not TTY");
        }

        let handle = TerminalHandle::new(outbound_tx);
        if let Some(timeout) = self.config.timeout {
            handle.timeout_after(timeout);
        }

        let mut events = interactive_input_enabled.then(EventStream::new);
        let result = self
            .run_loop(&handle, inbound_rx, &mut events, output, on_byte)
            .await;

        if interactive_input_enabled {
            crossterm::execute!(
                std::io::stdout(),
                crossterm::event::DisableMouseCapture
            )
            .ok();
            restore_terminal_mode();
            println!();
            eprintln!("✓ 已退出串口终端模式");
        }

        if handle.timed_out() {
            return Err(anyhow!(
                "{} timed out after {}s",
                self.config.timeout_label,
                self.config.timeout.unwrap_or_default().as_secs()
            ));
        }

        result
    }

    async fn run_loop<W, F>(
        &mut self,
        handle: &TerminalHandle,
        inbound_rx: &mut mpsc::UnboundedReceiver<Vec<u8>>,
        events: &mut Option<EventStream>,
        output: &mut W,
        on_byte: &mut F,
    ) -> anyhow::Result<()>
    where
        W: Write,
        F: FnMut(&TerminalHandle, u8) + Send,
    {
        while handle.is_running() {
            let mut wake_rx = handle.subscribe();
            let mut stop_deadline = Box::pin(async {
                if let Some(deadline) = handle.stop_deadline() {
                    sleep_until(deadline).await;
                } else {
                    futures::future::pending::<()>().await;
                }
            });
            let mut timeout_deadline = Box::pin(async {
                if let Some(deadline) = handle.timeout_deadline() {
                    sleep_until(deadline).await;
                } else {
                    futures::future::pending::<()>().await;
                }
            });

            tokio::select! {
                maybe_chunk = inbound_rx.recv() => {
                    match maybe_chunk {
                        Some(first_chunk) => {
                            // Drain all immediately-available chunks so that a
                            // complete TUI frame (which QEMU sends as many small
                            // UART-FIFO-sized bursts) is written to the host
                            // terminal in one flush rather than piece-by-piece.
                            // write_output does NOT flush at end; we flush once
                            // after the drain so the terminal renders atomically.
                            let mut all_bytes: Vec<u8> = Vec::with_capacity(first_chunk.len());
                            all_bytes.extend_from_slice(&first_chunk);
                            write_output(output, &first_chunk)?;
                            while let Ok(chunk) = inbound_rx.try_recv() {
                                all_bytes.extend_from_slice(&chunk);
                                write_output(output, &chunk)?;
                            }
                            output.flush()?;
                            for byte in all_bytes {
                                (on_byte)(handle, byte);
                            }
                        }
                        None => break,
                    }
                }
                maybe_event = async {
                    if let Some(events) = events.as_mut() {
                        events.next().await
                    } else {
                        futures::future::pending().await
                    }
                } => {
                    match maybe_event {
                        Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                            match self.key_processor.process_key(key)? {
                                TerminalAction::SendBytes(bytes) => {
                                    handle.send(bytes)?;
                                }
                                TerminalAction::ExitRequested => {
                                    eprintln!("\r\nExit by: Ctrl+A+x");
                                    handle.stop();
                                }
                                TerminalAction::Noop => {}
                            }
                        }
                        Some(Ok(Event::Mouse(mouse))) => {
                            if let Some(bytes) = encode_mouse_event(mouse) {
                                handle.send(bytes)?;
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(err)) => return Err(err.into()),
                        None => break,
                    }
                }
                _ = &mut stop_deadline => {
                    handle.stop();
                }
                _ = &mut timeout_deadline => {
                    handle.mark_timed_out();
                    handle.stop();
                }
                changed = wake_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}

impl TerminalHandle {
    fn new(outbound_tx: mpsc::UnboundedSender<Vec<u8>>) -> Self {
        let (wake_tx, _wake_rx) = watch::channel(0u64);
        Self {
            inner: Arc::new(TerminalState {
                running: AtomicBool::new(true),
                timed_out: AtomicBool::new(false),
                stop_deadline: Mutex::new(None),
                timeout_deadline: Mutex::new(None),
                outbound_tx,
                wake_version: AtomicU64::new(0),
                wake_tx,
            }),
        }
    }

    pub fn stop(&self) {
        self.inner.running.store(false, Ordering::Release);
        self.wake();
    }

    pub fn stop_after(&self, duration: Duration) {
        let mut deadline = self.inner.stop_deadline.lock().unwrap();
        if deadline.is_none() {
            *deadline = Some(Instant::now() + duration);
            drop(deadline);
            self.wake();
        }
    }

    pub fn timeout_after(&self, duration: Duration) {
        let mut deadline = self.inner.timeout_deadline.lock().unwrap();
        if deadline.is_none() {
            *deadline = Some(Instant::now() + duration);
            drop(deadline);
            self.wake();
        }
    }

    pub fn send_after(&self, duration: Duration, bytes: Vec<u8>) {
        let handle = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            if handle.is_running() {
                let _ = handle.send(bytes);
            }
        });
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Acquire)
    }

    fn send(&self, bytes: Vec<u8>) -> io::Result<()> {
        self.inner
            .outbound_tx
            .send(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "terminal transport closed"))
    }

    fn timed_out(&self) -> bool {
        self.inner.timed_out.load(Ordering::Acquire)
    }

    fn mark_timed_out(&self) {
        self.inner.timed_out.store(true, Ordering::Release);
    }

    fn stop_deadline(&self) -> Option<Instant> {
        *self.inner.stop_deadline.lock().unwrap()
    }

    fn timeout_deadline(&self) -> Option<Instant> {
        *self.inner.timeout_deadline.lock().unwrap()
    }

    fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.wake_tx.subscribe()
    }

    fn wake(&self) {
        let version = self.inner.wake_version.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.inner.wake_tx.send(version);
    }
}

impl KeyProcessor {
    fn new(intercept_exit_sequence: bool) -> Self {
        Self {
            intercept_exit_sequence,
            state: KeySequenceState::Normal,
        }
    }

    fn process_key(&mut self, key: KeyEvent) -> io::Result<TerminalAction> {
        if !self.intercept_exit_sequence {
            return encode_key_event(key);
        }

        match self.state {
            KeySequenceState::Normal => {
                if key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.state = KeySequenceState::CtrlAPressed;
                    Ok(TerminalAction::Noop)
                } else {
                    encode_key_event(key)
                }
            }
            KeySequenceState::CtrlAPressed => {
                if key.code == KeyCode::Char('x') {
                    self.state = KeySequenceState::Normal;
                    Ok(TerminalAction::ExitRequested)
                } else if key.code == KeyCode::Char('a') {
                    Ok(TerminalAction::Noop)
                } else {
                    self.state = KeySequenceState::Normal;
                    let mut bytes = vec![0x01];
                    match encode_key_event(key)? {
                        TerminalAction::SendBytes(mut key_bytes) => {
                            bytes.append(&mut key_bytes);
                            Ok(TerminalAction::SendBytes(bytes))
                        }
                        TerminalAction::ExitRequested | TerminalAction::Noop => {
                            Ok(TerminalAction::SendBytes(bytes))
                        }
                    }
                }
            }
        }
    }
}

/// Encode a crossterm MouseEvent as SGR mouse bytes (`\x1b[<Cb;Cx;CyM/m`).
///
/// These bytes are forwarded verbatim to the guest serial port.  The guest's
/// TUI (e.g. jcode/crossterm) has already sent `\x1b[?1006h` to enable SGR
/// mode, so the host terminal is emitting SGR events; we just re-encode them
/// from the parsed `MouseEvent` back into the wire format.
pub fn encode_mouse_event(mouse: MouseEvent) -> Option<Vec<u8>> {
    // Compute SGR Cb: button bits + modifier bits
    let button_bits: u8 = match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => 0,
        MouseEventKind::Down(MouseButton::Middle) | MouseEventKind::Up(MouseButton::Middle) => 1,
        MouseEventKind::Down(MouseButton::Right) | MouseEventKind::Up(MouseButton::Right) => 2,
        MouseEventKind::Drag(MouseButton::Left) => 32,
        MouseEventKind::Drag(MouseButton::Middle) => 33,
        MouseEventKind::Drag(MouseButton::Right) => 34,
        MouseEventKind::Moved => 35,
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        MouseEventKind::ScrollLeft => 66,
        MouseEventKind::ScrollRight => 67,
    };

    let mut cb = button_bits;
    if mouse.modifiers.contains(KeyModifiers::SHIFT) {
        cb |= 4;
    }
    if mouse.modifiers.contains(KeyModifiers::ALT) {
        cb |= 8;
    }
    if mouse.modifiers.contains(KeyModifiers::CONTROL) {
        cb |= 16;
    }

    // SGR uses 1-based coordinates
    let cx = mouse.column + 1;
    let cy = mouse.row + 1;

    // Final byte: 'M' for press/move, 'm' for release
    let final_byte = match mouse.kind {
        MouseEventKind::Up(_) => b'm',
        _ => b'M',
    };

    Some(format!("\x1b[<{cb};{cx};{cy}{}", final_byte as char).into_bytes())
}

pub fn encode_key_event(key: KeyEvent) -> io::Result<TerminalAction> {
    let mut bytes = Vec::new();
    match key.code {
        KeyCode::Char(c) => handle_character_key(c, key.modifiers, &mut bytes),
        KeyCode::Enter => handle_enter_key(key.modifiers, &mut bytes),
        KeyCode::Backspace => handle_backspace_key(key.modifiers, &mut bytes),
        KeyCode::Tab => handle_tab_key(key.modifiers, &mut bytes),
        KeyCode::BackTab => bytes.extend_from_slice(&[0x1b, b'[', b'Z']),
        KeyCode::Esc => {
            if key.modifiers.contains(KeyModifiers::ALT) {
                bytes.extend_from_slice(&[0x1b, 0x1b]);
            } else {
                bytes.push(0x1b);
            }
        }
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
            handle_arrow_key(key.code, key.modifiers, &mut bytes)
        }
        KeyCode::Home | KeyCode::End => handle_home_end_key(key.code, key.modifiers, &mut bytes),
        KeyCode::PageUp | KeyCode::PageDown => handle_page_key(key.code, key.modifiers, &mut bytes),
        KeyCode::Insert => handle_insert_key(key.modifiers, &mut bytes),
        KeyCode::Delete => handle_delete_key(key.modifiers, &mut bytes),
        KeyCode::F(n) => handle_function_key(n, key.modifiers, &mut bytes),
        KeyCode::Null
        | KeyCode::CapsLock
        | KeyCode::ScrollLock
        | KeyCode::NumLock
        | KeyCode::PrintScreen
        | KeyCode::Pause
        | KeyCode::Menu
        | KeyCode::KeypadBegin
        | KeyCode::Media(_)
        | KeyCode::Modifier(_) => {}
    }

    if bytes.is_empty() {
        Ok(TerminalAction::Noop)
    } else {
        Ok(TerminalAction::SendBytes(bytes))
    }
}

/// Write a chunk of guest output to the host terminal.
///
/// We write the whole chunk in one call and only flush on newline boundaries
/// (for interactive shell use).  The caller is responsible for flushing after
/// draining all immediately-available chunks so that TUI frames are sent to
/// the host terminal in one shot rather than byte-by-byte.
fn write_output(output: &mut impl Write, chunk: &[u8]) -> io::Result<()> {
    output.write_all(chunk)?;
    if chunk.contains(&b'\n') {
        output.flush()?;
    }
    Ok(())
}

fn handle_character_key(c: char, modifiers: KeyModifiers, bytes: &mut Vec<u8>) {
    if modifiers.contains(KeyModifiers::CONTROL) {
        let ctrl_char = match c {
            'a'..='z' => ((c as u8 - b'a') + 1) as char,
            'A'..='Z' => ((c as u8 - b'A') + 1) as char,
            '2' => '\x00',
            '3' => '\x1b',
            '4' => '\x1c',
            '5' => '\x1d',
            '6' => '\x1e',
            '7' => '\x1f',
            '8' => '\x7f',
            '?' => '\x7f',
            '[' => '\x1b',
            ']' => '\x1d',
            '^' => '\x1e',
            '_' => '\x1f',
            _ => c,
        };
        bytes.push(ctrl_char as u8);
    } else if modifiers.contains(KeyModifiers::ALT) {
        bytes.push(0x1b);
        bytes.push(c as u8);
    } else {
        bytes.push(c as u8);
    }
}

fn handle_enter_key(modifiers: KeyModifiers, bytes: &mut Vec<u8>) {
    if modifiers.contains(KeyModifiers::ALT) {
        bytes.extend_from_slice(&[0x1b, b'\r']);
    } else if modifiers.contains(KeyModifiers::SHIFT) {
        bytes.extend_from_slice(&[0x1b, b'[', b'Z']);
    } else {
        bytes.push(b'\r');
    }
}

fn handle_backspace_key(modifiers: KeyModifiers, bytes: &mut Vec<u8>) {
    if modifiers.contains(KeyModifiers::ALT) {
        bytes.extend_from_slice(&[0x1b, 0x7f]);
    } else if modifiers.contains(KeyModifiers::CONTROL) {
        bytes.push(b'\x08');
    } else {
        bytes.push(0x7f);
    }
}

fn handle_tab_key(modifiers: KeyModifiers, bytes: &mut Vec<u8>) {
    if modifiers.contains(KeyModifiers::SHIFT) {
        bytes.extend_from_slice(&[0x1b, b'[', b'Z']);
    } else if modifiers.contains(KeyModifiers::ALT) {
        bytes.extend_from_slice(&[0x1b, b'\t']);
    } else if modifiers.contains(KeyModifiers::CONTROL) {
        bytes.extend_from_slice(&[0x1b, b'[', b'I']);
    } else {
        bytes.push(b'\t');
    }
}

fn handle_arrow_key(key: KeyCode, modifiers: KeyModifiers, bytes: &mut Vec<u8>) {
    let base_sequence = match key {
        KeyCode::Up => b'A',
        KeyCode::Down => b'B',
        KeyCode::Right => b'C',
        KeyCode::Left => b'D',
        _ => return,
    };

    if modifiers.contains(KeyModifiers::ALT) {
        bytes.extend_from_slice(&[0x1b, b'[', b'1', b';', b'3', base_sequence]);
    } else if modifiers.contains(KeyModifiers::SHIFT) {
        bytes.extend_from_slice(&[0x1b, b'[', b'1', b';', b'2', base_sequence]);
    } else if modifiers.contains(KeyModifiers::CONTROL) {
        bytes.extend_from_slice(&[0x1b, b'[', b'1', b';', b'5', base_sequence]);
    } else {
        bytes.extend_from_slice(&[0x1b, b'[', base_sequence]);
    }
}

fn handle_home_end_key(key: KeyCode, modifiers: KeyModifiers, bytes: &mut Vec<u8>) {
    let base_sequence = match key {
        KeyCode::Home => b'H',
        KeyCode::End => b'F',
        _ => return,
    };

    if modifiers.contains(KeyModifiers::SHIFT) {
        bytes.extend_from_slice(&[0x1b, b'[', b'1', b';', b'2', base_sequence]);
    } else if modifiers.contains(KeyModifiers::CONTROL) {
        bytes.extend_from_slice(&[0x1b, b'[', b'1', b';', b'5', base_sequence]);
    } else {
        bytes.extend_from_slice(&[0x1b, b'[', base_sequence]);
    }
}

fn handle_page_key(key: KeyCode, modifiers: KeyModifiers, bytes: &mut Vec<u8>) {
    let base_sequence = match key {
        KeyCode::PageUp => b'5',
        KeyCode::PageDown => b'6',
        _ => return,
    };

    if modifiers.contains(KeyModifiers::SHIFT) {
        bytes.extend_from_slice(&[0x1b, b'[', base_sequence, b';', b'2', b'~']);
    } else if modifiers.contains(KeyModifiers::CONTROL) {
        bytes.extend_from_slice(&[0x1b, b'[', base_sequence, b';', b'5', b'~']);
    } else if modifiers.contains(KeyModifiers::ALT) {
        bytes.extend_from_slice(&[0x1b, b'[', base_sequence, b';', b'3', b'~']);
    } else {
        bytes.extend_from_slice(&[0x1b, b'[', base_sequence, b'~']);
    }
}

fn handle_insert_key(modifiers: KeyModifiers, bytes: &mut Vec<u8>) {
    if modifiers.contains(KeyModifiers::SHIFT) {
        bytes.extend_from_slice(&[0x1b, b'[', b'2', b';', b'2', b'~']);
    } else if modifiers.contains(KeyModifiers::CONTROL) {
        bytes.extend_from_slice(&[0x1b, b'[', b'2', b';', b'5', b'~']);
    } else {
        bytes.extend_from_slice(&[0x1b, b'[', b'2', b'~']);
    }
}

fn handle_delete_key(modifiers: KeyModifiers, bytes: &mut Vec<u8>) {
    if modifiers.contains(KeyModifiers::SHIFT) {
        bytes.extend_from_slice(&[0x1b, b'[', b'3', b';', b'2', b'~']);
    } else if modifiers.contains(KeyModifiers::CONTROL) {
        bytes.extend_from_slice(&[0x1b, b'[', b'3', b';', b'5', b'~']);
    } else if modifiers.contains(KeyModifiers::ALT) {
        bytes.extend_from_slice(&[0x1b, b'[', b'3', b';', b'3', b'~']);
    } else {
        bytes.extend_from_slice(&[0x1b, b'[', b'3', b'~']);
    }
}

fn handle_function_key(n: u8, modifiers: KeyModifiers, bytes: &mut Vec<u8>) {
    match n {
        1..=4 => {
            let f_char = match n {
                1 => b'P',
                2 => b'Q',
                3 => b'R',
                4 => b'S',
                _ => return,
            };

            if modifiers.contains(KeyModifiers::SHIFT) {
                bytes.extend_from_slice(&[0x1b, b'[', b'1', b';', b'2', f_char]);
            } else if modifiers.contains(KeyModifiers::ALT) {
                bytes.extend_from_slice(&[0x1b, b'[', b'1', b';', b'3', f_char]);
            } else if modifiers.contains(KeyModifiers::CONTROL) {
                bytes.extend_from_slice(&[0x1b, b'[', b'1', b';', b'5', f_char]);
            } else {
                bytes.extend_from_slice(&[0x1b, b'O', f_char]);
            }
        }
        5..=12 => {
            let f_sequence = match n {
                5 => &b"15"[..],
                6 => &b"17"[..],
                7 => &b"18"[..],
                8 => &b"19"[..],
                9 => &b"20"[..],
                10 => &b"21"[..],
                11 => &b"23"[..],
                12 => &b"24"[..],
                _ => return,
            };

            if modifiers.contains(KeyModifiers::SHIFT) {
                bytes.extend_from_slice(&[0x1b, b'[']);
                bytes.extend_from_slice(f_sequence);
                bytes.extend_from_slice(b";2~");
            } else if modifiers.contains(KeyModifiers::ALT) {
                bytes.extend_from_slice(&[0x1b, b'[']);
                bytes.extend_from_slice(f_sequence);
                bytes.extend_from_slice(b";3~");
            } else if modifiers.contains(KeyModifiers::CONTROL) {
                bytes.extend_from_slice(&[0x1b, b'[']);
                bytes.extend_from_slice(f_sequence);
                bytes.extend_from_slice(b";5~");
            } else {
                bytes.extend_from_slice(&[0x1b, b'[']);
                bytes.extend_from_slice(f_sequence);
                bytes.push(b'~');
            }
        }
        13..=24 => {
            let f_num = n + 12;
            let f_str = f_num.to_string();

            if modifiers.contains(KeyModifiers::SHIFT) {
                bytes.extend_from_slice(&[0x1b, b'[']);
                bytes.extend_from_slice(f_str.as_bytes());
                bytes.extend_from_slice(b";2~");
            } else if modifiers.contains(KeyModifiers::ALT) {
                bytes.extend_from_slice(&[0x1b, b'[']);
                bytes.extend_from_slice(f_str.as_bytes());
                bytes.extend_from_slice(b";3~");
            } else if modifiers.contains(KeyModifiers::CONTROL) {
                bytes.extend_from_slice(&[0x1b, b'[']);
                bytes.extend_from_slice(f_str.as_bytes());
                bytes.extend_from_slice(b";5~");
            } else {
                bytes.extend_from_slice(&[0x1b, b'[']);
                bytes.extend_from_slice(f_str.as_bytes());
                bytes.push(b'~');
            }
        }
        _ => {}
    }
}

pub fn restore_terminal_mode() {
    let _ = disable_raw_mode();
    let _ = Command::new("stty").arg("echo").arg("icanon").status();
    let _ = io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Cursor, Write},
        sync::{Arc, Mutex},
        time::Duration,
    };

    use super::{KeyProcessor, TerminalAction, TerminalHandle, encode_key_event, write_output};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc;

    struct FlushCountingWriter {
        buf: Vec<u8>,
        flushes: usize,
    }

    impl FlushCountingWriter {
        fn new() -> Self {
            Self {
                buf: Vec::new(),
                flushes: 0,
            }
        }
    }

    impl Write for FlushCountingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buf.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn ctrl_a_x_requests_exit() {
        let mut processor = KeyProcessor::new(true);
        assert_eq!(
            processor
                .process_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))
                .unwrap(),
            TerminalAction::Noop
        );
        assert_eq!(
            processor
                .process_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
                .unwrap(),
            TerminalAction::ExitRequested
        );
    }

    #[test]
    fn ctrl_a_then_other_key_replays_ctrl_a_and_key() {
        let mut processor = KeyProcessor::new(true);
        let _ = processor.process_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(
            processor
                .process_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE))
                .unwrap(),
            TerminalAction::SendBytes(vec![0x01, b'b'])
        );
    }

    #[test]
    fn plain_key_encoding_is_preserved() {
        assert_eq!(
            encode_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap(),
            TerminalAction::SendBytes(vec![b'\r'])
        );
        assert_eq!(
            encode_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)).unwrap(),
            TerminalAction::SendBytes(vec![0x1b, b'[', b'A'])
        );
    }

    #[test]
    fn write_output_flushes_on_newline_boundaries() {
        let mut writer = FlushCountingWriter::new();

        write_output(&mut writer, b"line1\nline2").unwrap();

        assert_eq!(writer.buf, b"line1\nline2");
        assert_eq!(writer.flushes, 2);
    }

    #[test]
    fn write_output_preserves_existing_carriage_returns() {
        let mut writer = FlushCountingWriter::new();

        write_output(&mut writer, b"line1\r\nline2").unwrap();

        assert_eq!(writer.buf, b"line1\r\nline2");
        assert_eq!(writer.flushes, 2);
    }

    #[test]
    fn stop_after_does_not_mark_timeout() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let handle = TerminalHandle::new(tx);
        handle.stop_after(Duration::from_millis(10));
        assert!(!handle.timed_out());
        assert!(handle.stop_deadline().is_some());
        assert!(handle.timeout_deadline().is_none());
    }

    #[test]
    fn timeout_after_sets_timeout_deadline_only() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let handle = TerminalHandle::new(tx);
        handle.timeout_after(Duration::from_millis(10));
        assert!(!handle.timed_out());
        assert!(handle.stop_deadline().is_none());
        assert!(handle.timeout_deadline().is_some());
    }

    #[tokio::test]
    async fn non_tty_mode_consumes_output_without_event_stream() {
        let terminal = super::AsyncTerminal::new(super::TerminalConfig {
            intercept_exit_sequence: true,
            timeout: None,
            timeout_label: "test terminal".to_string(),
        });
        let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel();
        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        let mut written = Vec::new();

        inbound_tx.send(b"hello".to_vec()).unwrap();
        drop(inbound_tx);

        terminal
            .run_with_output_mode(
                &mut inbound_rx,
                outbound_tx,
                &mut Cursor::new(&mut written),
                &mut move |_handle, byte| seen_clone.lock().unwrap().push(byte),
                false,
            )
            .await
            .unwrap();

        drop(outbound_rx);
        assert_eq!(written, b"hello");
        assert_eq!(*seen.lock().unwrap(), b"hello");
    }

    #[tokio::test]
    async fn non_tty_mode_still_honors_timeout() {
        let terminal = super::AsyncTerminal::new(super::TerminalConfig {
            intercept_exit_sequence: true,
            timeout: Some(Duration::from_millis(10)),
            timeout_label: "test terminal".to_string(),
        });
        let (_inbound_tx, mut inbound_rx) = mpsc::unbounded_channel();
        let (outbound_tx, _outbound_rx) = mpsc::unbounded_channel();
        let mut written = Vec::new();

        let err = terminal
            .run_with_output_mode(
                &mut inbound_rx,
                outbound_tx,
                &mut Cursor::new(&mut written),
                &mut |_handle, _byte| {},
                false,
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("timed out"));
    }
}

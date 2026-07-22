//! Rockchip RK3588 FIQ debugger UART support.

use core::ptr::NonNull;

use heapless::{String, Vec};
use rdif_serial::{
    Config, ConfigError, RxSample, SerialEventSet, SplitUart, UartInfo, UartParts, UartPort,
};

use super::{
    Kind, Ns16550, Ns16550Irq,
    registers::{
        LineStatusFlags, UART_DLH, UART_DLL, UART_FCR, UART_IER, UART_IER_RDI, UART_LCR,
        UART_LCR_DLAB, UART_LCR_WLEN8, UART_LSR, UART_LSR_DR, UART_LSR_THRE, UART_MCR, UART_RBR,
    },
};
use crate::{PollingUart, SerialEvent, TransferError};

pub const ROCKCHIP_FIQ_RK3588_UART_CLOCK: u32 = 24_000_000;
pub const ROCKCHIP_FIQ_DEFAULT_BAUDRATE: u32 = 1_500_000;

const REG_SHIFT: usize = 2;
const DEBUG_MAX: usize = 64;
const HISTORY_MAX: usize = 16;
const UART_USR: u8 = 0x1f;
const UART_SRR: u8 = 0x22;
const UART_USR_TX_FIFO_NOT_FULL: u32 = 0x02;

pub type CommandString = String<DEBUG_MAX>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RockchipFiqConfig {
    pub serial_id: u32,
    pub baudrate: u32,
    pub clock_hz: u32,
    pub irq_mode_enabled: bool,
    pub debug_enable: bool,
    pub console_enable: bool,
}

impl Default for RockchipFiqConfig {
    fn default() -> Self {
        Self {
            serial_id: 0,
            baudrate: ROCKCHIP_FIQ_DEFAULT_BAUDRATE,
            clock_hz: ROCKCHIP_FIQ_RK3588_UART_CLOCK,
            irq_mode_enabled: false,
            debug_enable: true,
            console_enable: true,
        }
    }
}

impl RockchipFiqConfig {
    pub fn normalised(mut self) -> Self {
        self.baudrate = normalise_baudrate(self.baudrate);
        if self.clock_hz == 0 {
            self.clock_hz = ROCKCHIP_FIQ_RK3588_UART_CLOCK;
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FiqCommand {
    Pc,
    Regs,
    AllRegs,
    Bt,
    Pcsr,
    Irqs,
    Kmsg,
    Version,
    Ps,
    SysRq(Option<u8>),
    Reboot(Option<CommandString>),
    Reset(Option<CommandString>),
    Kgdb,
    Cpu,
    CpuSwitch(u32),
    Sleep,
    NoSleep,
    Console,
    Help,
    Unknown(CommandString),
}

impl FiqCommand {
    pub fn parse(cmd: &str) -> Self {
        let cmd = cmd.trim();
        match cmd {
            "help" | "?" => Self::Help,
            "pc" => Self::Pc,
            "regs" => Self::Regs,
            "allregs" => Self::AllRegs,
            "bt" => Self::Bt,
            "pcsr" => Self::Pcsr,
            "irqs" => Self::Irqs,
            "kmsg" => Self::Kmsg,
            "version" => Self::Version,
            "ps" => Self::Ps,
            "sysrq" => Self::SysRq(None),
            "kgdb" => Self::Kgdb,
            "cpu" => Self::Cpu,
            "sleep" => Self::Sleep,
            "nosleep" => Self::NoSleep,
            "console" => Self::Console,
            _ if cmd.starts_with("sysrq ") => Self::SysRq(cmd.as_bytes().get(6).copied()),
            _ if cmd.starts_with("reboot") => Self::Reboot(command_arg(cmd, "reboot")),
            _ if cmd.starts_with("reset") => Self::Reset(command_arg(cmd, "reset")),
            _ if cmd.starts_with("cpu ") => cmd[4..]
                .trim()
                .parse::<u32>()
                .map(Self::CpuSwitch)
                .unwrap_or_else(|_| Self::Unknown(command_string(cmd))),
            _ => Self::Unknown(command_string(cmd)),
        }
    }

    pub fn needs_irq_helper(&self) -> bool {
        matches!(
            self,
            Self::Ps | Self::SysRq(_) | Self::Reboot(_) | Self::Kgdb | Self::Unknown(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FiqDebuggerEvent {
    ConsoleByte(u8),
    OutputByte(u8),
    EnterDebugger,
    ExitToConsole,
    Command(FiqCommand),
    NeedIrqHelper,
}

pub struct FiqDebugger {
    debug_enable: bool,
    console_enable: bool,
    no_sleep: bool,
    line: CommandString,
    history: Vec<CommandString, HISTORY_MAX>,
    history_cursor: Option<usize>,
    prev3: u8,
    prev2: u8,
    prev1: u8,
    escape_state: u8,
    last_newline: u8,
}

impl FiqDebugger {
    pub fn new(config: RockchipFiqConfig) -> Self {
        Self {
            debug_enable: config.debug_enable,
            console_enable: config.console_enable,
            no_sleep: false,
            line: CommandString::new(),
            history: Vec::new(),
            history_cursor: None,
            prev3: 0,
            prev2: 0,
            prev1: 0,
            escape_state: 0,
            last_newline: 0,
        }
    }

    pub fn debug_enabled(&self) -> bool {
        self.debug_enable
    }

    pub fn console_enabled(&self) -> bool {
        self.console_enable
    }

    pub fn no_sleep(&self) -> bool {
        self.no_sleep
    }

    pub fn current_line(&self) -> &str {
        self.line.as_str()
    }

    pub fn handle_byte(&mut self, byte: u8, emit: &mut impl FnMut(FiqDebuggerEvent)) {
        let is_break = self.update_break_detector(byte);

        if !self.debug_enable {
            if byte == b'\r' || byte == b'\n' {
                self.debug_enable = true;
                self.line.clear();
                self.prompt(emit);
            }
            return;
        }

        if is_break {
            self.enter_debugger(emit);
            return;
        }

        if self.console_enable {
            emit(FiqDebuggerEvent::ConsoleByte(byte));
            emit(FiqDebuggerEvent::NeedIrqHelper);
            return;
        }

        if self.handle_escape(byte, emit) {
            return;
        }

        match byte {
            9 => self.complete_unique_prefix(emit),
            8 | 127 => self.backspace(emit),
            b'\r' | b'\n' => self.submit_newline(byte, emit),
            b' '..=126 if self.line.len() < DEBUG_MAX - 1 => {
                let _ = self.line.push(byte as char);
                emit(FiqDebuggerEvent::OutputByte(byte));
            }
            _ => {}
        }
    }

    fn update_break_detector(&mut self, byte: u8) -> bool {
        let is_break = byte == b'q'
            && self.prev1 == b'i'
            && self.prev2 == b'f'
            && self.prev3 != b'_'
            && self.prev3 != b' ';
        self.prev3 = self.prev2;
        self.prev2 = self.prev1;
        self.prev1 = byte;
        is_break
    }

    fn enter_debugger(&mut self, emit: &mut impl FnMut(FiqDebuggerEvent)) {
        self.debug_enable = true;
        self.console_enable = false;
        self.line.clear();
        self.history_cursor = None;
        emit(FiqDebuggerEvent::EnterDebugger);
        self.emit_str("\nWelcome to fiq debugger mode\n", emit);
        self.emit_str("Enter ? to get command help\n", emit);
        self.prompt(emit);
    }

    fn handle_escape(&mut self, byte: u8, emit: &mut impl FnMut(FiqDebuggerEvent)) -> bool {
        match (self.escape_state, byte) {
            (0, 0x1b) => {
                self.escape_state = 1;
                true
            }
            (1, b'[') => {
                self.escape_state = 2;
                true
            }
            (2, b'A') => {
                self.escape_state = 0;
                self.history_up(emit);
                true
            }
            (2, b'B') => {
                self.escape_state = 0;
                self.history_down(emit);
                true
            }
            (2, b'C' | b'D') => {
                self.escape_state = 0;
                true
            }
            (1 | 2, _) => {
                self.escape_state = 0;
                false
            }
            _ => false,
        }
    }

    fn submit_newline(&mut self, byte: u8, emit: &mut impl FnMut(FiqDebuggerEvent)) {
        if byte == b'\r' || (byte == b'\n' && self.last_newline != b'\r') {
            emit(FiqDebuggerEvent::OutputByte(b'\r'));
            emit(FiqDebuggerEvent::OutputByte(b'\n'));
        }
        self.last_newline = byte;

        if self.line.is_empty() {
            self.prompt(emit);
            return;
        }

        let line = self.line.clone();
        self.line.clear();
        self.history_cursor = None;
        self.push_history(line.clone());

        let command = FiqCommand::parse(line.as_str());
        match command {
            FiqCommand::Sleep => {
                self.no_sleep = false;
                self.emit_str("enabling sleep\n", emit);
            }
            FiqCommand::NoSleep => {
                self.no_sleep = true;
                self.emit_str("disabling sleep\n", emit);
            }
            FiqCommand::Console => {
                self.emit_str("console mode\n", emit);
                self.console_enable = true;
                emit(FiqDebuggerEvent::ExitToConsole);
            }
            FiqCommand::Help => self.emit_help(emit),
            _ => {}
        }

        let needs_irq_helper = command.needs_irq_helper();
        emit(FiqDebuggerEvent::Command(command));
        if needs_irq_helper || (self.debug_enable && !self.no_sleep) {
            emit(FiqDebuggerEvent::NeedIrqHelper);
        }
        if !self.console_enable {
            self.prompt(emit);
        }
    }

    fn push_history(&mut self, line: CommandString) {
        if self.history.last() == Some(&line) {
            return;
        }
        if self.history.len() == HISTORY_MAX {
            self.history.remove(0);
        }
        let _ = self.history.push(line);
    }

    fn history_up(&mut self, emit: &mut impl FnMut(FiqDebuggerEvent)) {
        if self.history.is_empty() {
            return;
        }
        let next = self
            .history_cursor
            .map(|idx| idx.saturating_sub(1))
            .unwrap_or(self.history.len() - 1);
        self.history_cursor = Some(next);
        let line = self.history[next].clone();
        self.replace_line(line, emit);
    }

    fn history_down(&mut self, emit: &mut impl FnMut(FiqDebuggerEvent)) {
        let Some(idx) = self.history_cursor else {
            return;
        };
        if idx + 1 < self.history.len() {
            self.history_cursor = Some(idx + 1);
            let line = self.history[idx + 1].clone();
            self.replace_line(line, emit);
        } else {
            self.history_cursor = None;
            self.replace_line(CommandString::new(), emit);
        }
    }

    fn replace_line(&mut self, line: CommandString, emit: &mut impl FnMut(FiqDebuggerEvent)) {
        while !self.line.is_empty() {
            self.backspace(emit);
        }
        self.line = line;
        let bytes: Vec<u8, DEBUG_MAX> = self.line.as_bytes().iter().copied().collect();
        for byte in bytes {
            emit(FiqDebuggerEvent::OutputByte(byte));
        }
    }

    fn backspace(&mut self, emit: &mut impl FnMut(FiqDebuggerEvent)) {
        if self.line.pop().is_some() {
            emit(FiqDebuggerEvent::OutputByte(8));
            emit(FiqDebuggerEvent::OutputByte(b' '));
            emit(FiqDebuggerEvent::OutputByte(8));
        }
    }

    fn complete_unique_prefix(&mut self, emit: &mut impl FnMut(FiqDebuggerEvent)) {
        let mut found = None;
        for cmd in COMMANDS {
            if cmd.starts_with(self.line.as_str()) {
                if found.is_some() {
                    return;
                }
                found = Some(*cmd);
            }
        }

        let Some(cmd) = found else {
            return;
        };
        if cmd.len() <= self.line.len() {
            return;
        }
        for &byte in &cmd.as_bytes()[self.line.len()..] {
            if self.line.push(byte as char).is_ok() {
                emit(FiqDebuggerEvent::OutputByte(byte));
            }
        }
    }

    fn prompt(&self, emit: &mut impl FnMut(FiqDebuggerEvent)) {
        self.emit_str("> ", emit);
    }

    fn emit_help(&self, emit: &mut impl FnMut(FiqDebuggerEvent)) {
        self.emit_str(
            "pc regs allregs bt reboot sleep nosleep console cpu reset irqs kmsg version ps sysrq \
             kgdb\n",
            emit,
        );
    }

    fn emit_str(&self, s: &str, emit: &mut impl FnMut(FiqDebuggerEvent)) {
        for byte in s.bytes() {
            emit(FiqDebuggerEvent::OutputByte(byte));
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RockchipFiqPort {
    base: usize,
}

impl RockchipFiqPort {
    pub const fn new(base: usize) -> Self {
        Self { base }
    }

    pub fn base_addr(&self) -> usize {
        self.base
    }

    fn reg_addr(&self, reg: u8) -> usize {
        self.base + ((reg as usize) << REG_SHIFT)
    }

    fn read_u32(&self, reg: u8) -> u32 {
        unsafe { (self.reg_addr(reg) as *const u32).read_volatile() }
    }

    fn write_u32(&self, reg: u8, value: u32) {
        unsafe { (self.reg_addr(reg) as *mut u32).write_volatile(value) }
    }

    fn init_debug_port(&self, baudrate: u32) {
        if self.read_reg(UART_LSR) & UART_LSR_DR != 0 {
            let _ = self.read_reg(UART_RBR);
        }

        let dll = match normalise_baudrate(baudrate) {
            1_500_000 => 0x01,
            _ => 0x0d,
        };

        self.write_reg(UART_SRR, 0x07);
        for _ in 0..1024 {
            core::hint::spin_loop();
        }
        self.write_reg(UART_MCR, 0x10);
        self.write_reg(UART_LCR, UART_LCR_DLAB | UART_LCR_WLEN8);
        self.write_reg(UART_DLL, dll);
        self.write_reg(UART_DLH, 0);
        self.write_reg(UART_LCR, UART_LCR_WLEN8);
        self.write_reg(UART_IER, UART_IER_RDI);
        self.write_reg(UART_FCR, 0x01);
        self.write_reg(UART_MCR, 0);
    }
}

impl Kind for RockchipFiqPort {
    fn read_reg(&self, reg: u8) -> u8 {
        let mut value = (self.read_u32(reg) & 0xff) as u8;
        if reg == UART_LSR && self.read_u32(UART_USR) & UART_USR_TX_FIFO_NOT_FULL != 0 {
            value |= UART_LSR_THRE;
        }
        value
    }

    fn write_reg(&self, reg: u8, val: u8) {
        self.write_u32(reg, val as u32);
    }

    fn get_base(&self) -> usize {
        self.base
    }

    fn set_baudrate(&self, _clock_freq: u32, baudrate: u32) -> Result<(), ConfigError> {
        if !matches!(baudrate, 115_200 | 1_500_000) {
            return Err(ConfigError::InvalidBaudrate);
        }
        self.init_debug_port(baudrate);
        Ok(())
    }

    fn baudrate(&self, _clock_freq: u32) -> u32 {
        let lcr = self.read_reg(UART_LCR);
        self.write_reg(UART_LCR, lcr | UART_LCR_DLAB);
        let dll = self.read_reg(UART_DLL);
        let dlh = self.read_reg(UART_DLH);
        self.write_reg(UART_LCR, lcr);

        match (dll, dlh) {
            (0x01, 0) => 1_500_000,
            (0x0d, 0) => 115_200,
            _ => 0,
        }
    }
}

impl Ns16550<RockchipFiqPort> {
    pub fn new_rockchip_fiq(base: NonNull<u8>, clock_freq: u32) -> Self {
        let base = RockchipFiqPort::new(base.as_ptr() as usize);
        Self {
            base,
            clock_freq,
            saved_lsr: LineStatusFlags::empty(),
        }
    }
}

pub struct RockchipFiqSerial {
    serial: Ns16550<RockchipFiqPort>,
    debugger: FiqDebugger,
    config: RockchipFiqConfig,
}

impl RockchipFiqSerial {
    pub fn new(base: NonNull<u8>, config: RockchipFiqConfig) -> Self {
        let config = config.normalised();
        let port = RockchipFiqPort::new(base.as_ptr() as usize);
        port.init_debug_port(config.baudrate);
        let serial = Ns16550::new_rockchip_fiq(base, config.clock_hz);
        Self {
            serial,
            debugger: FiqDebugger::new(config),
            config,
        }
    }

    pub fn config(&self) -> RockchipFiqConfig {
        self.config
    }

    pub fn handle_fiq_byte(&mut self, byte: u8, emit: &mut impl FnMut(FiqDebuggerEvent)) {
        self.debugger.handle_byte(byte, emit);
    }

    pub fn debugger(&self) -> &FiqDebugger {
        &self.debugger
    }

    pub fn debugger_mut(&mut self) -> &mut FiqDebugger {
        &mut self.debugger
    }
}

impl UartPort for RockchipFiqSerial {
    fn startup(&mut self, config: &Config) -> Result<(), ConfigError> {
        self.serial.startup(config)
    }

    fn shutdown(&mut self) {
        self.serial.shutdown()
    }

    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        self.serial.set_config(config)
    }

    fn read_rx(&mut self) -> Option<RxSample> {
        UartPort::read_rx(&mut self.serial)
    }

    fn write_tx(&mut self, bytes: &[u8]) -> usize {
        UartPort::write_tx(&mut self.serial, bytes)
    }

    fn tx_idle(&mut self) -> bool {
        self.serial.tx_idle()
    }

    fn mask_all(&mut self) {
        UartPort::mask_all(&mut self.serial);
    }

    fn rearm(&mut self, sources: SerialEventSet) -> SerialEventSet {
        UartPort::rearm(&mut self.serial, sources)
    }
}

impl SplitUart for RockchipFiqSerial {
    type Port = Self;
    type Irq = Ns16550Irq<RockchipFiqPort>;

    fn runtime_info(&self) -> UartInfo {
        UartInfo {
            name: "Rockchip FIQ Debugger UART",
            register_base: self.serial.base.get_base(),
            initial_baudrate: self.config.baudrate,
        }
    }

    fn split(self) -> UartParts<Self::Port, Self::Irq> {
        let irq = Ns16550Irq {
            base: self.serial.base,
        };
        UartParts::new(self, irq)
    }
}

impl PollingUart for RockchipFiqSerial {
    fn poll_status(&mut self) -> SerialEvent {
        self.serial.poll_status()
    }

    fn write_byte(&mut self, byte: u8) {
        self.serial.write_byte(byte);
    }

    fn read_byte(&mut self, status: SerialEvent) -> Option<Result<u8, TransferError>> {
        self.serial.read_byte(status)
    }
}

const COMMANDS: &[&str] = &[
    "pc", "regs", "allregs", "bt", "reboot", "pcsr", "sleep", "nosleep", "console", "cpu", "reset",
    "irqs", "kmsg", "version", "ps", "sysrq", "kgdb",
];

fn command_arg(cmd: &str, prefix: &str) -> Option<CommandString> {
    let arg = cmd[prefix.len()..].trim();
    if arg.is_empty() {
        None
    } else {
        Some(command_string(arg))
    }
}

fn command_string(value: &str) -> CommandString {
    let mut out = CommandString::new();
    let _ = out.push_str(value);
    out
}

fn normalise_baudrate(baudrate: u32) -> u32 {
    match baudrate {
        115_200 | 1_500_000 => baudrate,
        _ => 115_200,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(debugger: &mut FiqDebugger, bytes: &[u8]) -> heapless::Vec<FiqDebuggerEvent, 64> {
        let mut out = heapless::Vec::new();
        for &byte in bytes {
            debugger.handle_byte(byte, &mut |event| {
                let _ = out.push(event);
            });
        }
        out
    }

    #[test]
    fn fiq_word_enters_debugger_unless_prefixed_by_space_or_underscore() {
        let mut debugger = FiqDebugger::new(RockchipFiqConfig {
            debug_enable: true,
            console_enable: true,
            ..RockchipFiqConfig::default()
        });

        let events = feed(&mut debugger, b"fiq");
        assert!(events.contains(&FiqDebuggerEvent::EnterDebugger));
        assert!(!debugger.console_enabled());

        let mut debugger = FiqDebugger::new(RockchipFiqConfig {
            debug_enable: true,
            console_enable: true,
            ..RockchipFiqConfig::default()
        });
        let events = feed(&mut debugger, b" fiq");
        assert!(!events.contains(&FiqDebuggerEvent::EnterDebugger));
        assert!(debugger.console_enabled());

        let mut debugger = FiqDebugger::new(RockchipFiqConfig {
            debug_enable: true,
            console_enable: true,
            ..RockchipFiqConfig::default()
        });
        let events = feed(&mut debugger, b"_fiq");
        assert!(!events.contains(&FiqDebuggerEvent::EnterDebugger));
        assert!(debugger.console_enabled());
    }

    #[test]
    fn newline_enables_debugger_when_debugging_is_disabled() {
        let mut debugger = FiqDebugger::new(RockchipFiqConfig {
            debug_enable: false,
            console_enable: false,
            ..RockchipFiqConfig::default()
        });

        let events = feed(&mut debugger, b"\r");
        assert!(debugger.debug_enabled());
        assert!(
            events
                .iter()
                .any(|event| matches!(event, FiqDebuggerEvent::OutputByte(b'>')))
        );
    }

    #[test]
    fn command_line_parses_console_and_sleep_modes() {
        let mut debugger = FiqDebugger::new(RockchipFiqConfig {
            debug_enable: true,
            console_enable: false,
            ..RockchipFiqConfig::default()
        });

        let events = feed(&mut debugger, b"nosleep\r");
        assert!(debugger.no_sleep());
        assert!(events.contains(&FiqDebuggerEvent::Command(FiqCommand::NoSleep)));

        let events = feed(&mut debugger, b"sleep\r");
        assert!(!debugger.no_sleep());
        assert!(events.contains(&FiqDebuggerEvent::Command(FiqCommand::Sleep)));

        let events = feed(&mut debugger, b"console\r");
        assert!(debugger.console_enabled());
        assert!(events.contains(&FiqDebuggerEvent::ExitToConsole));
        assert!(events.contains(&FiqDebuggerEvent::Command(FiqCommand::Console)));
    }

    #[test]
    fn command_line_supports_backspace_history_and_tab_completion() {
        let mut debugger = FiqDebugger::new(RockchipFiqConfig {
            debug_enable: true,
            console_enable: false,
            ..RockchipFiqConfig::default()
        });

        let _ = feed(&mut debugger, b"nosleex\x08p\r");
        assert!(debugger.no_sleep());

        let _ = feed(&mut debugger, b"\x1b[A");
        assert_eq!(debugger.current_line(), "nosleep");

        let _ = feed(&mut debugger, b"\x1b[B");
        assert_eq!(debugger.current_line(), "");

        let _ = feed(&mut debugger, b"con\t");
        assert_eq!(debugger.current_line(), "console");
    }

    #[test]
    fn parser_covers_deferred_os_commands() {
        assert_eq!(FiqCommand::parse("ps"), FiqCommand::Ps);
        assert_eq!(FiqCommand::parse("sysrq"), FiqCommand::SysRq(None));
        assert_eq!(FiqCommand::parse("sysrq g"), FiqCommand::SysRq(Some(b'g')));
        assert_eq!(FiqCommand::parse("kgdb"), FiqCommand::Kgdb);

        let mut arg = CommandString::new();
        arg.push_str("bootloader").unwrap();
        assert_eq!(
            FiqCommand::parse("reboot bootloader"),
            FiqCommand::Reboot(Some(arg))
        );

        let mut unknown = CommandString::new();
        unknown.push_str("wat").unwrap();
        assert_eq!(FiqCommand::parse("wat"), FiqCommand::Unknown(unknown));
    }
}

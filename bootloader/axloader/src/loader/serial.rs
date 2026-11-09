//! Firmware UART identity output. No protocol borrow crosses network polling.
use alloc::{format, rc::Rc, string::String, vec::Vec};
use core::cell::RefCell;

use axloader::serial_path::console_matches;
use httpboot_protocol::{
    LoaderSerialStatus, SerialFlowControl, SerialParameters, SerialParity, SerialStopBits,
};
use uefi::{
    Event, Handle, Status,
    boot::{
        self, EventType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol, TimerTrigger,
        Tpl,
    },
    proto::{
        ProtocolPointer,
        console::serial::{ControlBits, Parity, Serial, StopBits},
        device_path::DevicePath,
    },
    runtime::{self, VariableVendor},
};

pub struct SerialBeacon {
    handle: Option<Handle>,
    timer: Option<Event>,
    force_parameters: bool,
    frame: Vec<u8>,
    offset: usize,
    write_failed: bool,
    startup_error: Option<String>,
    pub state: Rc<RefCell<LoaderSerialStatus>>,
}
impl SerialBeacon {
    pub fn new(serial_id: alloc::string::String) -> Self {
        let selection = select_console();
        crate::logln!(
            "loader_serial_probe: handles={} conout_matches={} selected={}",
            selection.handles,
            selection.conout_matches,
            u8::from(selection.handle.is_some())
        );
        let (handle, parameters, force_parameters, error) = match selection.handle {
            Some(handle) => match parameters(handle) {
                Ok((parameters, warning)) => {
                    (Some(handle), Some(parameters), warning.is_some(), warning)
                }
                Err(error) => (
                    Some(handle),
                    None,
                    false,
                    Some(format!("UART parameters unavailable: {error:?}")),
                ),
            },
            None => (None, None, false, selection.error),
        };
        let (timer, timer_error) = match timer() {
            Ok(timer) => (Some(timer), None),
            Err(error) => (None, Some(format!("UART timer unavailable: {error:?}"))),
        };
        let ready = handle.is_some() && timer.is_some() && parameters.is_some();
        let error = error.or(timer_error);
        let startup_error = error.clone();
        if ready {
            let parameters = parameters.expect("ready serial has parameters");
            crate::logln!(
                "loader_serial_ready: serial_id={serial_id} baud={} data={} parity={} stop={} \
                 flow={}",
                parameters.baud_rate,
                parameters.data_bits,
                parity_name(parameters.parity),
                stop_bits_name(parameters.stop_bits),
                flow_control_name(parameters.flow_control)
            );
        } else {
            crate::logln!(
                "loader_serial_unavailable: handles={} conout_matches={} reason={}",
                selection.handles,
                selection.conout_matches,
                error.as_deref().unwrap_or("UART identity unavailable")
            );
        }
        let frame = format!("\r\nAXLOADER-SERIAL/1 {serial_id}\r\n").into_bytes();
        Self {
            handle,
            timer,
            force_parameters,
            frame,
            offset: 0,
            write_failed: false,
            startup_error,
            state: Rc::new(RefCell::new(LoaderSerialStatus {
                serial_id,
                ready,
                parameters,
                binding: None,
                error: error.or_else(|| (!ready).then(|| "UART timer unavailable".into())),
            })),
        }
    }
    pub fn progress(&mut self) {
        if self.state.borrow().binding.is_some() || self.state.borrow().parameters.is_none() {
            self.offset = 0;
            return;
        }
        if self.handle.is_none() || self.timer.is_none() {
            return;
        }
        let due = self
            .timer
            .as_ref()
            .is_some_and(|event| boot::check_event(event).is_ok());
        // A failed write is retried on the next beacon tick. Successful partial
        // writes continue immediately so the frame remains bounded and timely.
        if (self.offset == 0 || self.write_failed) && !due {
            return;
        }
        let Some(handle) = self.handle else {
            return;
        };
        let end = (self.offset + 4).min(self.frame.len());
        let parameters = if self.force_parameters {
            self.state.borrow().parameters
        } else {
            None
        };
        match write_chunk(handle, &self.frame[self.offset..end], parameters) {
            Ok(size) => {
                // A timeout reports firmware-written bytes; treat that count as
                // a hint bounded by the bytes requested for this chunk.
                let remaining = self.frame.len().saturating_sub(self.offset);
                self.offset += size.min(remaining);
                if self.offset == self.frame.len() {
                    self.offset = 0;
                }
                if self.write_failed {
                    let mut state = self.state.borrow_mut();
                    state.ready = true;
                    state.error = self.startup_error.clone();
                    self.write_failed = false;
                }
            }
            Err(error) => {
                let mut state = self.state.borrow_mut();
                state.ready = false;
                state.error = Some(format!("UART write failed: {error:?}"));
                self.write_failed = true;
            }
        }
    }
}
impl Drop for SerialBeacon {
    fn drop(&mut self) {
        if let Some(event) = self.timer.take() {
            let _ = boot::close_event(event);
        }
    }
}
fn timer() -> uefi::Result<Event> {
    // SAFETY: a timer without a callback retains no Rust context or pointer.
    let event = unsafe { boot::create_event(EventType::TIMER, Tpl::CALLBACK, None, None) }?;
    if let Err(error) = boot::set_timer(&event, TimerTrigger::Periodic(2_500_000)) {
        let _ = boot::close_event(event);
        return Err(error);
    }
    Ok(event)
}
fn open<P: ProtocolPointer + ?Sized>(handle: Handle) -> uefi::Result<ScopedProtocol<P>> {
    // SAFETY: callers use a temporary guard at CALLBACK, outside ConOut or network
    // callbacks. No protocol pointer/reference escapes the call. GetProtocol keeps
    // the firmware console driver connected; guards close before lowering TPL.
    unsafe {
        boot::open_protocol::<P>(
            OpenProtocolParams {
                handle,
                agent: boot::image_handle(),
                controller: None,
            },
            OpenProtocolAttributes::GetProtocol,
        )
    }
}
fn parameters(handle: Handle) -> uefi::Result<(SerialParameters, Option<String>)> {
    // SAFETY: efi_main executes at APPLICATION. CALLBACK prevents competing serial
    // callbacks and is permitted for SerialIo. The guard restores TPL on every exit.
    let _tpl = unsafe { boot::raise_tpl(Tpl::CALLBACK) };
    let serial = open::<Serial>(handle)?;
    let mode = serial.io_mode();
    let mut warning = None;
    let flow_control = match serial.get_control_bits() {
        Ok(bits) if bits.contains(ControlBits::HARDWARE_FLOW_CONTROL_ENABLE) => {
            SerialFlowControl::RtsCts
        }
        Ok(_) => SerialFlowControl::None,
        Err(error) => {
            warning = Some(format!(
                "UART flow-control unavailable: {error:?}; using none"
            ));
            SerialFlowControl::None
        }
    };
    let parameters = SerialParameters {
        baud_rate: mode.baud_rate,
        data_bits: u8::try_from(mode.data_bits)
            .map_err(|_| uefi::Error::from(Status::UNSUPPORTED))?,
        parity: match mode.parity {
            Parity::NONE => SerialParity::None,
            Parity::ODD => SerialParity::Odd,
            Parity::EVEN => SerialParity::Even,
            Parity::MARK => SerialParity::Mark,
            Parity::SPACE => SerialParity::Space,
            _ => return Err(Status::UNSUPPORTED.into()),
        },
        stop_bits: match mode.stop_bits {
            StopBits::ONE => SerialStopBits::One,
            StopBits::ONE_FIVE => SerialStopBits::OnePointFive,
            StopBits::TWO => SerialStopBits::Two,
            _ => return Err(Status::UNSUPPORTED.into()),
        },
        flow_control,
    };
    parameters
        .validate()
        .map_err(|_| uefi::Error::from(Status::UNSUPPORTED))?;
    Ok((parameters, warning))
}
fn write_chunk(
    handle: Handle,
    bytes: &[u8],
    parameters: Option<SerialParameters>,
) -> uefi::Result<usize> {
    // SAFETY: same short-lived APPLICATION -> CALLBACK contract as parameters().
    let _tpl = unsafe { boot::raise_tpl(Tpl::CALLBACK) };
    let mut serial = open::<Serial>(handle)?;
    let original = *serial.io_mode();
    let mut bounded = original;
    if let Some(parameters) = parameters {
        bounded.baud_rate = parameters.baud_rate;
        bounded.data_bits = u32::from(parameters.data_bits);
        bounded.parity = match parameters.parity {
            SerialParity::None => Parity::NONE,
            SerialParity::Odd => Parity::ODD,
            SerialParity::Even => Parity::EVEN,
            SerialParity::Mark => Parity::MARK,
            SerialParity::Space => Parity::SPACE,
        };
        bounded.stop_bits = match parameters.stop_bits {
            SerialStopBits::One => StopBits::ONE,
            SerialStopBits::OnePointFive => StopBits::ONE_FIVE,
            SerialStopBits::Two => StopBits::TWO,
        };
    }
    bounded.timeout = 5_000;
    serial.set_attributes(&bounded)?;
    let result = serial.write(bytes);
    let restore = serial.set_attributes(&original);
    restore?;
    match result {
        Ok(()) => Ok(bytes.len()),
        Err(error) if error.status() == Status::TIMEOUT => Ok(*error.data()),
        Err(error) => Err(error.to_err_without_payload()),
    }
}
struct SerialSelection {
    handle: Option<Handle>,
    handles: usize,
    conout_matches: usize,
    error: Option<String>,
}

fn select_console() -> SerialSelection {
    let handles = match boot::find_handles::<Serial>() {
        Ok(handles) => handles,
        Err(error) => {
            return SerialSelection {
                handle: None,
                handles: 0,
                conout_matches: 0,
                error: Some(format!("SerialIo discovery failed: {error:?}")),
            };
        }
    };
    let console =
        runtime::get_variable_boxed(uefi::cstr16!("ConOut"), &VariableVendor::GLOBAL_VARIABLE)
            .ok()
            .map(|(console, _)| console);
    let mut selected = None;
    let mut conout_matches = 0;
    if let Some(console) = console.as_deref() {
        for handle in &handles {
            // SAFETY: DevicePath is borrowed only inside this firmware-call scope.
            let _tpl = unsafe { boot::raise_tpl(Tpl::CALLBACK) };
            let Ok(path) = open::<DevicePath>(*handle) else {
                continue;
            };
            if console_matches(console, path.as_bytes()) {
                conout_matches += 1;
                selected = Some(*handle);
            }
        }
    }
    let handle_count = handles.len();
    let (handle, error) = if conout_matches > 1 {
        (
            None,
            Some("UART selection failed: multiple ConOut matches".into()),
        )
    } else if selected.is_some() {
        (selected, None)
    } else {
        match handles.as_slice() {
            [handle] => (Some(*handle), None),
            [] => (
                None,
                Some(format!("UART selection failed: {:?}", Status::NOT_FOUND)),
            ),
            _ => (
                None,
                Some(format!("UART selection failed: {:?}", Status::UNSUPPORTED)),
            ),
        }
    };
    SerialSelection {
        handle,
        handles: handle_count,
        conout_matches,
        error,
    }
}

fn parity_name(parity: SerialParity) -> &'static str {
    match parity {
        SerialParity::None => "none",
        SerialParity::Odd => "odd",
        SerialParity::Even => "even",
        SerialParity::Mark => "mark",
        SerialParity::Space => "space",
    }
}

fn stop_bits_name(stop_bits: SerialStopBits) -> &'static str {
    match stop_bits {
        SerialStopBits::One => "1",
        SerialStopBits::OnePointFive => "1.5",
        SerialStopBits::Two => "2",
    }
}

fn flow_control_name(flow_control: SerialFlowControl) -> &'static str {
    match flow_control {
        SerialFlowControl::None => "none",
        SerialFlowControl::RtsCts => "rts-cts",
    }
}

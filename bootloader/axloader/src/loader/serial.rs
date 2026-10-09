//! Firmware UART identity output. No protocol borrow crosses network polling.
use alloc::{format, rc::Rc, vec::Vec};
use core::cell::RefCell;

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
    pub state: Rc<RefCell<LoaderSerialStatus>>,
}
impl SerialBeacon {
    pub fn new(serial_id: alloc::string::String) -> Self {
        let (handle, parameters, force_parameters, error) = match select_console() {
            Ok(handle) => match parameters(handle) {
                Ok(parameters) => (Some(handle), Some(parameters), false, None),
                Err(error) => (
                    Some(handle),
                    Some(default_serial_parameters()),
                    true,
                    Some(format!(
                        "UART parameters unavailable: {error:?}; using defaults 115200 8N1"
                    )),
                ),
            },
            Err(error) => (
                None,
                None,
                false,
                Some(format!("UART selection failed: {error:?}")),
            ),
        };
        let timer = timer().ok();
        let ready = handle.is_some() && timer.is_some();
        let frame = format!("\r\nAXLOADER-SERIAL/1 {serial_id}\r\n").into_bytes();
        Self {
            handle,
            timer,
            force_parameters,
            frame,
            offset: 0,
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
        if !self.state.borrow().ready || self.state.borrow().binding.is_some() {
            self.offset = 0;
            return;
        }
        let due = self
            .timer
            .as_ref()
            .is_some_and(|event| boot::check_event(event).is_ok());
        if self.offset == 0 && !due {
            return;
        }
        let Some(handle) = self.handle else {
            return;
        };
        let end = (self.offset + 4).min(self.frame.len());
        let parameters = self.force_parameters.then(default_serial_parameters);
        match write_chunk(handle, &self.frame[self.offset..end], parameters) {
            Ok(size) => {
                self.offset += size;
                if self.offset == self.frame.len() {
                    self.offset = 0;
                }
            }
            Err(error) => {
                let mut state = self.state.borrow_mut();
                state.ready = false;
                state.error = Some(format!("UART write failed: {error:?}"));
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
fn parameters(handle: Handle) -> uefi::Result<SerialParameters> {
    // SAFETY: efi_main executes at APPLICATION. CALLBACK prevents competing serial
    // callbacks and is permitted for SerialIo. The guard restores TPL on every exit.
    let _tpl = unsafe { boot::raise_tpl(Tpl::CALLBACK) };
    let serial = open::<Serial>(handle)?;
    let mode = serial.io_mode();
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
        flow_control: if serial
            .get_control_bits()?
            .contains(ControlBits::HARDWARE_FLOW_CONTROL_ENABLE)
        {
            SerialFlowControl::RtsCts
        } else {
            SerialFlowControl::None
        },
    };
    parameters
        .validate()
        .map_err(|_| uefi::Error::from(Status::UNSUPPORTED))?;
    Ok(parameters)
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
fn select_console() -> uefi::Result<Handle> {
    let handles = boot::find_handles::<Serial>()?;
    let console =
        runtime::get_variable_boxed(uefi::cstr16!("ConOut"), &VariableVendor::GLOBAL_VARIABLE)
            .ok()
            .map(|(console, _)| console);
    let mut selected = None;
    if let Some(console) = console.as_deref() {
        for handle in &handles {
            // SAFETY: DevicePath is borrowed only inside this firmware-call scope.
            let _tpl = unsafe { boot::raise_tpl(Tpl::CALLBACK) };
            let Ok(path) = open::<DevicePath>(*handle) else {
                continue;
            };
            if console_matches(console, path.as_bytes()) {
                if selected.is_some() {
                    return Err(Status::UNSUPPORTED.into());
                }
                selected = Some(*handle);
            }
        }
    }
    if let Some(selected) = selected {
        return Ok(selected);
    }
    match handles.as_slice() {
        [handle] => Ok(*handle),
        [] => Err(Status::NOT_FOUND.into()),
        _ => Err(Status::UNSUPPORTED.into()),
    }
}

fn default_serial_parameters() -> SerialParameters {
    SerialParameters {
        baud_rate: 115_200,
        data_bits: 8,
        parity: SerialParity::None,
        stop_bits: SerialStopBits::One,
        flow_control: SerialFlowControl::None,
    }
}
/// Match complete device-path nodes; UART attributes may use firmware defaults.
fn console_matches(console: &[u8], serial: &[u8]) -> bool {
    let Some(serial_nodes) = nodes(serial) else {
        return false;
    };
    let Some(console_nodes) = nodes(console) else {
        return false;
    };
    let prefix = serial_nodes
        .into_iter()
        .take_while(|n| n[0] != 0x7f)
        .collect::<Vec<_>>();
    if prefix.is_empty() {
        return false;
    }
    let mut start = 0;
    for (end, node) in console_nodes.iter().enumerate() {
        if node[0] != 0x7f {
            continue;
        }
        let instance = &console_nodes[start..end];
        if instance.len() >= prefix.len()
            && prefix.iter().zip(instance).all(|(a, b)| {
                if a.len() != b.len() {
                    false
                } else if a[0] == 3 && a[1] == 14 {
                    b[0] == 3 && b[1] == 14
                } else {
                    a == b
                }
            })
            && instance[prefix.len()..]
                .iter()
                .all(|n| n[0] == 3 && n[1] == 10)
        {
            return true;
        }
        start = end + 1;
    }
    false
}
fn nodes(mut bytes: &[u8]) -> Option<Vec<&[u8]>> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        let head = bytes.get(..4)?;
        let size = u16::from_le_bytes([head[2], head[3]]) as usize;
        if size < 4 {
            return None;
        }
        result.push(bytes.get(..size)?);
        bytes = bytes.get(size..)?;
    }
    Some(result)
}

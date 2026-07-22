//! SCMI platform-side message decoding and protocol dispatch.

use alloc::{format, vec, vec::Vec};

const CHANNEL_STATUS_OFFSET: usize = 0x04;
const LENGTH_OFFSET: usize = 0x14;
const MESSAGE_HEADER_OFFSET: usize = 0x18;
const PAYLOAD_OFFSET: usize = 0x1c;
const CHANNEL_FREE: u32 = 1;
const CHANNEL_ERROR: u32 = 2;
const MAX_MESSAGE_PAYLOAD: usize = 128;

const BASE_PROTOCOL: u8 = 0x10;
const CLOCK_PROTOCOL: u8 = 0x14;
const RESET_PROTOCOL: u8 = 0x16;

const PROTOCOL_VERSION: u8 = 0;
const PROTOCOL_ATTRIBUTES: u8 = 1;
const PROTOCOL_MESSAGE_ATTRIBUTES: u8 = 2;

const SUCCESS: i32 = 0;
const NOT_SUPPORTED: i32 = -1;
const INVALID_PARAMETERS: i32 = -2;
const DENIED: i32 = -3;
const NOT_FOUND: i32 = -4;
const OUT_OF_RANGE: i32 = -5;
const BUSY: i32 = -6;
const HARDWARE_ERROR: i32 = -9;
const PROTOCOL_ERROR: i32 = -10;

/// A validated SCMI command copied out of a shared-memory channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScmiServerRequest {
    message_header: u32,
    protocol_id: u8,
    message_id: u8,
    payload: Vec<u8>,
}

impl ScmiServerRequest {
    /// Returns the standard or vendor protocol identifier.
    pub const fn protocol_id(&self) -> u8 {
        self.protocol_id
    }

    /// Returns the protocol-local message identifier.
    pub const fn message_id(&self) -> u8 {
        self.message_id
    }

    /// Returns the request payload, excluding the transport message header.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

/// A complete synchronous SCMI response ready for shared-memory encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScmiServerResponse {
    message_header: u32,
    status: i32,
    payload: Vec<u8>,
}

impl ScmiServerResponse {
    fn success(request: &ScmiServerRequest, payload: Vec<u8>) -> Self {
        Self {
            message_header: request.message_header,
            status: SUCCESS,
            payload,
        }
    }

    fn error(request: &ScmiServerRequest, status: i32) -> Self {
        Self {
            message_header: request.message_header,
            status,
            payload: Vec::new(),
        }
    }

    /// Returns the signed SCMI status code.
    pub const fn status(&self) -> i32 {
        self.status
    }
}

/// Shared-memory framing failure detected before a command can be dispatched.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScmiServerCodecError {
    /// The supplied transport window is shorter than the SCMI header.
    #[error("SCMI shared-memory window is too short")]
    WindowTooShort,
    /// The advertised request or response length is invalid.
    #[error("invalid SCMI message length {length}")]
    InvalidLength {
        /// Length advertised in the shared-memory header.
        length: usize,
    },
    /// Only synchronous command messages are accepted by this server.
    #[error("unsupported SCMI message type {message_type}")]
    UnsupportedMessageType {
        /// Two-bit SCMI message type from the packed header.
        message_type: u8,
    },
}

/// Backend failure translated into one architectural SCMI status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScmiServerOperationError {
    /// The selected command is not implemented by the physical provider.
    #[error("operation is not supported")]
    NotSupported,
    /// The selected guest resource identifier does not exist.
    #[error("resource was not found")]
    NotFound,
    /// The agent does not own the requested operation.
    #[error("operation was denied")]
    Denied,
    /// A request value is outside the physical provider's range.
    #[error("value is out of range")]
    OutOfRange,
    /// The physical provider is temporarily busy.
    #[error("provider is busy")]
    Busy,
    /// The physical provider reported a hardware failure.
    #[error("provider hardware operation failed")]
    Hardware,
}

impl ScmiServerOperationError {
    const fn status(self) -> i32 {
        match self {
            Self::NotSupported => NOT_SUPPORTED,
            Self::NotFound => NOT_FOUND,
            Self::Denied => DENIED,
            Self::OutOfRange => OUT_OF_RANGE,
            Self::Busy => BUSY,
            Self::Hardware => HARDWARE_ERROR,
        }
    }
}

/// Lease-filtered provider operations made available to one SCMI agent.
pub trait ScmiServerBackend {
    /// Returns how many clock identifiers are visible to the agent.
    fn clock_count(&self) -> u32;

    /// Returns whether a visible clock is enabled.
    fn clock_enabled(&self, id: u32) -> Result<bool, ScmiServerOperationError>;

    /// Returns a visible clock's current rate.
    fn clock_rate(&self, id: u32) -> Result<u64, ScmiServerOperationError>;

    /// Changes a visible clock's rate.
    fn clock_set_rate(&self, id: u32, rate_hz: u64) -> Result<(), ScmiServerOperationError>;

    /// Applies the agent-visible enable state.
    fn clock_configure(&self, id: u32, enabled: bool) -> Result<(), ScmiServerOperationError>;

    /// Returns how many reset-domain identifiers are visible to the agent.
    fn reset_count(&self) -> u32;

    /// Returns whether a visible reset line is asserted.
    fn reset_asserted(&self, id: u32) -> Result<bool, ScmiServerOperationError>;

    /// Asserts or deasserts a visible reset line.
    fn reset_set(&self, id: u32, asserted: bool) -> Result<(), ScmiServerOperationError>;
}

/// Stateless dispatcher for the mandatory Base protocol and clock/reset v1.0.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScmiServer;

impl ScmiServer {
    /// Copies and validates one request from an SCMI shared-memory window.
    pub fn decode_request(window: &[u8]) -> Result<ScmiServerRequest, ScmiServerCodecError> {
        ensure_header(window)?;
        let length =
            read_u32(window, LENGTH_OFFSET).ok_or(ScmiServerCodecError::WindowTooShort)? as usize;
        if !(size_of::<u32>()..=size_of::<u32>() + MAX_MESSAGE_PAYLOAD).contains(&length) {
            return Err(ScmiServerCodecError::InvalidLength { length });
        }
        let payload_len = length - size_of::<u32>();
        let payload_end = PAYLOAD_OFFSET
            .checked_add(payload_len)
            .filter(|end| *end <= window.len())
            .ok_or(ScmiServerCodecError::InvalidLength { length })?;
        let message_header =
            read_u32(window, MESSAGE_HEADER_OFFSET).ok_or(ScmiServerCodecError::WindowTooShort)?;
        let message_type = ((message_header >> 8) & 0x3) as u8;
        if message_type != 0 {
            return Err(ScmiServerCodecError::UnsupportedMessageType { message_type });
        }
        Ok(ScmiServerRequest {
            message_header,
            protocol_id: ((message_header >> 10) & 0xff) as u8,
            message_id: (message_header & 0xff) as u8,
            payload: window[PAYLOAD_OFFSET..payload_end].to_vec(),
        })
    }

    /// Executes one validated request without retaining the transport buffer.
    pub fn execute(
        request: &ScmiServerRequest,
        backend: &dyn ScmiServerBackend,
    ) -> ScmiServerResponse {
        match request.protocol_id {
            BASE_PROTOCOL => execute_base(request, backend),
            CLOCK_PROTOCOL => execute_clock(request, backend),
            RESET_PROTOCOL => execute_reset(request, backend),
            _ => ScmiServerResponse::error(request, NOT_SUPPORTED),
        }
    }

    /// Writes a synchronous response and releases the shared-memory channel.
    pub fn encode_response(
        window: &mut [u8],
        response: &ScmiServerResponse,
    ) -> Result<(), ScmiServerCodecError> {
        ensure_header(window)?;
        let payload_len = size_of::<i32>()
            .checked_add(response.payload.len())
            .ok_or(ScmiServerCodecError::InvalidLength { length: usize::MAX })?;
        let length = size_of::<u32>()
            .checked_add(payload_len)
            .ok_or(ScmiServerCodecError::InvalidLength { length: usize::MAX })?;
        let end = PAYLOAD_OFFSET
            .checked_add(payload_len)
            .filter(|end| *end <= window.len() && length <= size_of::<u32>() + MAX_MESSAGE_PAYLOAD)
            .ok_or(ScmiServerCodecError::InvalidLength { length })?;
        write_u32(window, MESSAGE_HEADER_OFFSET, response.message_header)?;
        write_u32(window, LENGTH_OFFSET, length as u32)?;
        write_u32(window, PAYLOAD_OFFSET, response.status as u32)?;
        window[PAYLOAD_OFFSET + size_of::<i32>()..end].copy_from_slice(&response.payload);
        write_u32(window, CHANNEL_STATUS_OFFSET, CHANNEL_FREE)?;
        Ok(())
    }

    /// Marks a malformed transaction complete with a transport error.
    pub fn encode_protocol_error(window: &mut [u8]) -> Result<(), ScmiServerCodecError> {
        ensure_header(window)?;
        write_u32(window, LENGTH_OFFSET, 8)?;
        write_u32(window, PAYLOAD_OFFSET, PROTOCOL_ERROR as u32)?;
        write_u32(window, CHANNEL_STATUS_OFFSET, CHANNEL_FREE | CHANNEL_ERROR)?;
        Ok(())
    }
}

fn execute_base(
    request: &ScmiServerRequest,
    backend: &dyn ScmiServerBackend,
) -> ScmiServerResponse {
    match request.message_id {
        PROTOCOL_VERSION if request.payload.is_empty() => success_u32(request, 0x0002_0000),
        PROTOCOL_ATTRIBUTES if request.payload.is_empty() => {
            let protocols =
                u8::from(backend.clock_count() != 0) + u8::from(backend.reset_count() != 0);
            ScmiServerResponse::success(request, vec![protocols, 2, 0, 0])
        }
        PROTOCOL_MESSAGE_ATTRIBUTES => {
            message_attributes(request, |message| matches!(message, 0..=7))
        }
        3 if request.payload.is_empty() => ScmiServerResponse::success(request, fixed_name("AxVM")),
        4 if request.payload.is_empty() => {
            ScmiServerResponse::success(request, fixed_name("TGOSKits"))
        }
        5 if request.payload.is_empty() => success_u32(request, 1),
        6 => discover_protocols(request, backend),
        7 => discover_agent(request),
        _ => invalid_or_unsupported(request),
    }
}

fn discover_protocols(
    request: &ScmiServerRequest,
    backend: &dyn ScmiServerBackend,
) -> ScmiServerResponse {
    let Some(skip) = request_u32(request, 0, 4) else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    let mut protocols = Vec::new();
    if backend.clock_count() != 0 {
        protocols.push(CLOCK_PROTOCOL);
    }
    if backend.reset_count() != 0 {
        protocols.push(RESET_PROTOCOL);
    }
    let Ok(skip) = usize::try_from(skip) else {
        return ScmiServerResponse::error(request, OUT_OF_RANGE);
    };
    let selected = protocols.get(skip..).unwrap_or_default();
    let mut payload = Vec::with_capacity(4 + selected.len().next_multiple_of(4));
    payload.extend_from_slice(&(selected.len() as u32).to_le_bytes());
    payload.extend_from_slice(selected);
    payload.resize(4 + selected.len().next_multiple_of(4), 0);
    ScmiServerResponse::success(request, payload)
}

fn discover_agent(request: &ScmiServerRequest) -> ScmiServerResponse {
    let Some(agent) = request_u32(request, 0, 4) else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    let name = match agent {
        0 => "platform",
        1 => "OSPM",
        _ => return ScmiServerResponse::error(request, NOT_FOUND),
    };
    let mut payload = agent.to_le_bytes().to_vec();
    payload.extend_from_slice(&fixed_name(name));
    ScmiServerResponse::success(request, payload)
}

fn execute_clock(
    request: &ScmiServerRequest,
    backend: &dyn ScmiServerBackend,
) -> ScmiServerResponse {
    match request.message_id {
        PROTOCOL_VERSION if request.payload.is_empty() => success_u32(request, 0x0001_0000),
        PROTOCOL_ATTRIBUTES if request.payload.is_empty() => {
            let count = backend.clock_count().min(u32::from(u16::MAX)) as u16;
            let mut payload = count.to_le_bytes().to_vec();
            payload.extend_from_slice(&[0, 0]);
            ScmiServerResponse::success(request, payload)
        }
        PROTOCOL_MESSAGE_ATTRIBUTES => {
            message_attributes(request, |message| matches!(message, 0..=7))
        }
        3 => clock_attributes(request, backend),
        4 => clock_describe_rates(request, backend),
        5 => clock_rate_set(request, backend),
        6 => clock_rate_get(request, backend),
        7 => clock_config_set(request, backend),
        _ => invalid_or_unsupported(request),
    }
}

fn clock_attributes(
    request: &ScmiServerRequest,
    backend: &dyn ScmiServerBackend,
) -> ScmiServerResponse {
    let Some(id) = request_u32(request, 0, 4) else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    let enabled = match backend.clock_enabled(id) {
        Ok(enabled) => enabled,
        Err(error) => return operation_error(request, error),
    };
    let mut payload = u32::from(enabled).to_le_bytes().to_vec();
    payload.extend_from_slice(&fixed_name(&format!("vm-clock-{id}")));
    payload.extend_from_slice(&0_u32.to_le_bytes());
    ScmiServerResponse::success(request, payload)
}

fn clock_describe_rates(
    request: &ScmiServerRequest,
    backend: &dyn ScmiServerBackend,
) -> ScmiServerResponse {
    let Some(id) = request_u32(request, 0, 8) else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    let Some(rate_index) = request_u32(request, 4, 8) else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    if rate_index != 0 {
        return ScmiServerResponse::error(request, OUT_OF_RANGE);
    }
    let maximum = match backend.clock_rate(id) {
        Ok(rate) if rate != 0 => rate,
        Ok(_) => return ScmiServerResponse::error(request, OUT_OF_RANGE),
        Err(error) => return operation_error(request, error),
    };
    let flags = 3_u32 | (1 << 12);
    let mut payload = flags.to_le_bytes().to_vec();
    for rate in [1_u64, maximum, 1_u64] {
        payload.extend_from_slice(&rate.to_le_bytes());
    }
    ScmiServerResponse::success(request, payload)
}

fn clock_rate_set(
    request: &ScmiServerRequest,
    backend: &dyn ScmiServerBackend,
) -> ScmiServerResponse {
    if request.payload.len() != 16 {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    }
    let Some(flags) = read_payload_u32(request, 0) else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    if flags & !0x0f != 0 || flags & 1 != 0 {
        return ScmiServerResponse::error(request, NOT_SUPPORTED);
    }
    let (Some(id), Some(rate_low), Some(rate_high)) = (
        read_payload_u32(request, 4),
        read_payload_u32(request, 8),
        read_payload_u32(request, 12),
    ) else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    let rate = u64::from(rate_low) | (u64::from(rate_high) << 32);
    if rate == 0 {
        return ScmiServerResponse::error(request, OUT_OF_RANGE);
    }
    match backend.clock_set_rate(id, rate) {
        Ok(()) => ScmiServerResponse::success(request, Vec::new()),
        Err(error) => operation_error(request, error),
    }
}

fn clock_rate_get(
    request: &ScmiServerRequest,
    backend: &dyn ScmiServerBackend,
) -> ScmiServerResponse {
    let Some(id) = request_u32(request, 0, 4) else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    match backend.clock_rate(id) {
        Ok(rate) => ScmiServerResponse::success(request, rate.to_le_bytes().to_vec()),
        Err(error) => operation_error(request, error),
    }
}

fn clock_config_set(
    request: &ScmiServerRequest,
    backend: &dyn ScmiServerBackend,
) -> ScmiServerResponse {
    if request.payload.len() != 8 {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    }
    let (Some(id), Some(attributes)) = (read_payload_u32(request, 0), read_payload_u32(request, 4))
    else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    if attributes > 1 {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    }
    match backend.clock_configure(id, attributes == 1) {
        Ok(()) => ScmiServerResponse::success(request, Vec::new()),
        Err(error) => operation_error(request, error),
    }
}

fn execute_reset(
    request: &ScmiServerRequest,
    backend: &dyn ScmiServerBackend,
) -> ScmiServerResponse {
    match request.message_id {
        PROTOCOL_VERSION if request.payload.is_empty() => success_u32(request, 0x0001_0000),
        PROTOCOL_ATTRIBUTES if request.payload.is_empty() => {
            success_u32(request, backend.reset_count().min(u32::from(u16::MAX)))
        }
        PROTOCOL_MESSAGE_ATTRIBUTES => {
            message_attributes(request, |message| matches!(message, 0..=4))
        }
        3 => reset_attributes(request, backend),
        4 => reset_command(request, backend),
        _ => invalid_or_unsupported(request),
    }
}

fn reset_attributes(
    request: &ScmiServerRequest,
    backend: &dyn ScmiServerBackend,
) -> ScmiServerResponse {
    let Some(id) = request_u32(request, 0, 4) else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    if let Err(error) = backend.reset_asserted(id) {
        return operation_error(request, error);
    }
    let mut payload = 0_u32.to_le_bytes().to_vec();
    payload.extend_from_slice(&0_u32.to_le_bytes());
    payload.extend_from_slice(&fixed_name(&format!("vm-reset-{id}")));
    ScmiServerResponse::success(request, payload)
}

fn reset_command(
    request: &ScmiServerRequest,
    backend: &dyn ScmiServerBackend,
) -> ScmiServerResponse {
    if request.payload.len() != 12 {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    }
    let (Some(id), Some(flags)) = (read_payload_u32(request, 0), read_payload_u32(request, 4))
    else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    if flags & !0x3 != 0 {
        return ScmiServerResponse::error(request, NOT_SUPPORTED);
    }
    if flags == 0x3 {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    }
    let Some(reset_state) = read_payload_u32(request, 8) else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    if reset_state != 0 {
        return ScmiServerResponse::error(request, NOT_SUPPORTED);
    }
    let operation = if flags & 1 != 0 {
        backend
            .reset_set(id, true)
            .and_then(|()| backend.reset_set(id, false))
    } else {
        backend.reset_set(id, flags & 2 != 0)
    };
    match operation {
        Ok(()) => ScmiServerResponse::success(request, Vec::new()),
        Err(error) => operation_error(request, error),
    }
}

fn message_attributes(
    request: &ScmiServerRequest,
    supported: impl FnOnce(u8) -> bool,
) -> ScmiServerResponse {
    let Some(message) = request_u32(request, 0, 4) else {
        return ScmiServerResponse::error(request, INVALID_PARAMETERS);
    };
    if message <= u32::from(u8::MAX) && supported(message as u8) {
        success_u32(request, 0)
    } else {
        ScmiServerResponse::error(request, NOT_FOUND)
    }
}

fn invalid_or_unsupported(request: &ScmiServerRequest) -> ScmiServerResponse {
    if request.payload.len() > MAX_MESSAGE_PAYLOAD {
        ScmiServerResponse::error(request, INVALID_PARAMETERS)
    } else {
        ScmiServerResponse::error(request, NOT_SUPPORTED)
    }
}

fn operation_error(
    request: &ScmiServerRequest,
    error: ScmiServerOperationError,
) -> ScmiServerResponse {
    ScmiServerResponse::error(request, error.status())
}

fn success_u32(request: &ScmiServerRequest, value: u32) -> ScmiServerResponse {
    ScmiServerResponse::success(request, value.to_le_bytes().to_vec())
}

fn request_u32(request: &ScmiServerRequest, offset: usize, expected_len: usize) -> Option<u32> {
    (request.payload.len() == expected_len)
        .then(|| read_payload_u32(request, offset))
        .flatten()
}

fn read_payload_u32(request: &ScmiServerRequest, offset: usize) -> Option<u32> {
    read_u32(&request.payload, offset)
}

fn fixed_name(name: &str) -> Vec<u8> {
    let mut result = vec![0; 16];
    let bytes = name.as_bytes();
    let length = bytes.len().min(result.len().saturating_sub(1));
    result[..length].copy_from_slice(&bytes[..length]);
    result
}

fn ensure_header(window: &[u8]) -> Result<(), ScmiServerCodecError> {
    if window.len() < PAYLOAD_OFFSET + size_of::<i32>() {
        return Err(ScmiServerCodecError::WindowTooShort);
    }
    Ok(())
}

fn read_u32(window: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(size_of::<u32>())?;
    let bytes = window.get(offset..end)?;
    let mut value = [0; size_of::<u32>()];
    value.copy_from_slice(bytes);
    Some(u32::from_le_bytes(value))
}

fn write_u32(window: &mut [u8], offset: usize, value: u32) -> Result<(), ScmiServerCodecError> {
    let end = offset
        .checked_add(size_of::<u32>())
        .ok_or(ScmiServerCodecError::WindowTooShort)?;
    let target = window
        .get_mut(offset..end)
        .ok_or(ScmiServerCodecError::WindowTooShort)?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;

    struct Backend {
        rate: Cell<u64>,
        reset: Cell<bool>,
    }

    impl ScmiServerBackend for Backend {
        fn clock_count(&self) -> u32 {
            1
        }

        fn clock_enabled(&self, id: u32) -> Result<bool, ScmiServerOperationError> {
            (id == 0)
                .then_some(true)
                .ok_or(ScmiServerOperationError::NotFound)
        }

        fn clock_rate(&self, id: u32) -> Result<u64, ScmiServerOperationError> {
            (id == 0)
                .then(|| self.rate.get())
                .ok_or(ScmiServerOperationError::NotFound)
        }

        fn clock_set_rate(&self, id: u32, rate_hz: u64) -> Result<(), ScmiServerOperationError> {
            if id != 0 {
                return Err(ScmiServerOperationError::NotFound);
            }
            self.rate.set(rate_hz);
            Ok(())
        }

        fn clock_configure(&self, id: u32, _enabled: bool) -> Result<(), ScmiServerOperationError> {
            (id == 0)
                .then_some(())
                .ok_or(ScmiServerOperationError::NotFound)
        }

        fn reset_count(&self) -> u32 {
            1
        }

        fn reset_asserted(&self, id: u32) -> Result<bool, ScmiServerOperationError> {
            (id == 0)
                .then(|| self.reset.get())
                .ok_or(ScmiServerOperationError::NotFound)
        }

        fn reset_set(&self, id: u32, asserted: bool) -> Result<(), ScmiServerOperationError> {
            if id != 0 {
                return Err(ScmiServerOperationError::NotFound);
            }
            self.reset.set(asserted);
            Ok(())
        }
    }

    #[test]
    fn clock_rate_request_preserves_header_and_releases_channel() {
        let backend = Backend {
            rate: Cell::new(200_000_000),
            reset: Cell::new(false),
        };
        let header = 6 | (u32::from(CLOCK_PROTOCOL) << 10) | (37 << 18);
        let mut window = request_window(header, &0_u32.to_le_bytes());

        let request = ScmiServer::decode_request(&window).unwrap();
        let response = ScmiServer::execute(&request, &backend);
        ScmiServer::encode_response(&mut window, &response).unwrap();

        assert_eq!(read_u32(&window, MESSAGE_HEADER_OFFSET), Some(header));
        assert_eq!(read_u32(&window, CHANNEL_STATUS_OFFSET), Some(CHANNEL_FREE));
        assert_eq!(read_u32(&window, LENGTH_OFFSET), Some(16));
        assert_eq!(read_u32(&window, PAYLOAD_OFFSET), Some(SUCCESS as u32));
        assert_eq!(
            u64::from_le_bytes(
                window[PAYLOAD_OFFSET + 4..PAYLOAD_OFFSET + 12]
                    .try_into()
                    .unwrap()
            ),
            200_000_000
        );
    }

    #[test]
    fn explicit_reset_request_changes_only_the_selected_domain() {
        let backend = Backend {
            rate: Cell::new(24_000_000),
            reset: Cell::new(false),
        };
        let mut payload = 0_u32.to_le_bytes().to_vec();
        payload.extend_from_slice(&2_u32.to_le_bytes());
        payload.extend_from_slice(&0_u32.to_le_bytes());
        let header = 4 | (u32::from(RESET_PROTOCOL) << 10);
        let window = request_window(header, &payload);

        let request = ScmiServer::decode_request(&window).unwrap();
        let response = ScmiServer::execute(&request, &backend);

        assert_eq!(response.status(), SUCCESS);
        assert!(backend.reset.get());
    }

    #[test]
    fn malformed_length_is_rejected_before_backend_dispatch() {
        let mut window = vec![0; 64];
        write_u32(&mut window, LENGTH_OFFSET, 256).unwrap();

        assert_eq!(
            ScmiServer::decode_request(&window),
            Err(ScmiServerCodecError::InvalidLength { length: 256 })
        );
    }

    #[test]
    fn linux_probe_discovers_only_the_implemented_protocols() {
        let backend = Backend {
            rate: Cell::new(200_000_000),
            reset: Cell::new(false),
        };
        let response = execute_request(&backend, BASE_PROTOCOL, PROTOCOL_ATTRIBUTES, &[]);
        assert_eq!(response.status(), SUCCESS);
        assert_eq!(response.payload, vec![2, 2, 0, 0]);

        let response = execute_request(&backend, BASE_PROTOCOL, 6, &0_u32.to_le_bytes());
        assert_eq!(response.status(), SUCCESS);
        assert_eq!(
            &response.payload[..6],
            &[2, 0, 0, 0, CLOCK_PROTOCOL, RESET_PROTOCOL]
        );

        let response = execute_request(&backend, CLOCK_PROTOCOL, 3, &0_u32.to_le_bytes());
        assert_eq!(response.status(), SUCCESS);
        assert_eq!(read_u32(&response.payload, 0), Some(1));
    }

    #[test]
    fn clock_rate_set_updates_only_a_valid_visible_clock() {
        let backend = Backend {
            rate: Cell::new(200_000_000),
            reset: Cell::new(false),
        };
        let mut payload = 0_u32.to_le_bytes().to_vec();
        payload.extend_from_slice(&0_u32.to_le_bytes());
        payload.extend_from_slice(&375_000_u32.to_le_bytes());
        payload.extend_from_slice(&0_u32.to_le_bytes());

        let response = execute_request(&backend, CLOCK_PROTOCOL, 5, &payload);

        assert_eq!(response.status(), SUCCESS);
        assert_eq!(backend.rate.get(), 375_000);
    }

    #[test]
    fn clock_rate_set_rejects_rates_below_the_advertised_minimum() {
        let backend = Backend {
            rate: Cell::new(200_000_000),
            reset: Cell::new(false),
        };
        let mut payload = 0_u32.to_le_bytes().to_vec();
        payload.extend_from_slice(&0_u32.to_le_bytes());
        payload.extend_from_slice(&0_u32.to_le_bytes());
        payload.extend_from_slice(&0_u32.to_le_bytes());

        let response = execute_request(&backend, CLOCK_PROTOCOL, 5, &payload);

        assert_eq!(response.status(), OUT_OF_RANGE);
        assert_eq!(backend.rate.get(), 200_000_000);
    }

    #[test]
    fn unsupported_reset_state_does_not_touch_the_backend() {
        let backend = Backend {
            rate: Cell::new(24_000_000),
            reset: Cell::new(false),
        };
        let mut payload = 0_u32.to_le_bytes().to_vec();
        payload.extend_from_slice(&2_u32.to_le_bytes());
        payload.extend_from_slice(&1_u32.to_le_bytes());

        let response = execute_request(&backend, RESET_PROTOCOL, 4, &payload);

        assert_eq!(response.status(), NOT_SUPPORTED);
        assert!(!backend.reset.get());
    }

    fn execute_request(
        backend: &Backend,
        protocol: u8,
        message: u8,
        payload: &[u8],
    ) -> ScmiServerResponse {
        let header = u32::from(message) | (u32::from(protocol) << 10);
        let window = request_window(header, payload);
        let request = ScmiServer::decode_request(&window).unwrap();
        ScmiServer::execute(&request, backend)
    }

    fn request_window(header: u32, payload: &[u8]) -> Vec<u8> {
        let mut window = vec![0; 256];
        write_u32(&mut window, LENGTH_OFFSET, (4 + payload.len()) as u32).unwrap();
        write_u32(&mut window, MESSAGE_HEADER_OFFSET, header).unwrap();
        window[PAYLOAD_OFFSET..PAYLOAD_OFFSET + payload.len()].copy_from_slice(payload);
        window
    }
}

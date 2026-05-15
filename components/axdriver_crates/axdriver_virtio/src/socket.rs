use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use ax_driver_vsock::{VsockConnId, VsockDriverEvent, VsockDriverOps};
use virtio_drivers::{
    Hal,
    device::socket::{
        DisconnectReason, VirtIOSocket, VsockAddr, VsockConnectionManager as InnerDev, VsockEvent,
        VsockEventType,
    },
    transport::Transport,
};

use crate::as_dev_err;

const DEFAULT_RX_BUFFER_CAPACITY: u32 = 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MappedConnId {
    peer_addr: VsockAddr,
    local_port: u32,
}

impl MappedConnId {
    const fn new(peer_addr: VsockAddr, local_port: u32) -> Self {
        Self {
            peer_addr,
            local_port,
        }
    }

    const fn peer_addr(self) -> VsockAddr {
        self.peer_addr
    }

    const fn local_port(self) -> u32 {
        self.local_port
    }

    fn into_driver_conn_id(self) -> VsockConnId {
        VsockConnId {
            peer_addr: ax_driver_vsock::VsockAddr {
                cid: self.peer_addr.cid,
                port: self.peer_addr.port,
            },
            local_port: self.local_port,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionOperation {
    Connect,
    Send,
    Receive,
    ReceiveAvailable,
    Disconnect,
    Abort,
}

impl ConnectionOperation {
    const fn requires_non_empty_buffer(self) -> bool {
        matches!(self, Self::Send | Self::Receive)
    }

    const fn refreshes_credit_after_completion(self) -> bool {
        matches!(self, Self::Receive | Self::ReceiveAvailable)
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Send => "send",
            Self::Receive => "receive",
            Self::ReceiveAvailable => "receive_available",
            Self::Disconnect => "disconnect",
            Self::Abort => "abort",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OperationRequest {
    conn: MappedConnId,
    operation: ConnectionOperation,
}

impl OperationRequest {
    const fn new(conn: MappedConnId, operation: ConnectionOperation) -> Self {
        Self { conn, operation }
    }

    const fn conn(self) -> MappedConnId {
        self.conn
    }

    const fn operation(self) -> ConnectionOperation {
        self.operation
    }
}

fn normalize_operation_request(request: OperationRequest) -> DevResult<OperationRequest> {
    let conn = validate_mapped_conn(request.conn(), request.operation())?;
    Ok(OperationRequest::new(conn, request.operation()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ListenRequest {
    src_port: u32,
}

impl ListenRequest {
    const fn new(src_port: u32) -> Self {
        Self { src_port }
    }

    const fn src_port(self) -> u32 {
        self.src_port
    }

    const fn is_valid(self) -> bool {
        self.src_port != 0
    }
}

fn normalize_listen_request(request: ListenRequest) -> Option<ListenRequest> {
    validate_port(request.src_port()).ok().map(|_| request)
}

fn should_listen_on_port(request: ListenRequest) -> bool {
    request.is_valid()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TranslatedEventKind {
    ConnectionRequest,
    Connected,
    Received(usize),
    Disconnected,
    CreditUpdate,
    Unknown,
}

impl TranslatedEventKind {
    const fn is_connection_event(self) -> bool {
        matches!(
            self,
            Self::ConnectionRequest | Self::Connected | Self::Disconnected
        )
    }

    const fn is_data_event(self) -> bool {
        matches!(self, Self::Received(_) | Self::CreditUpdate)
    }

    const fn should_surface_to_driver(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TranslatedEvent {
    conn_id: VsockConnId,
    kind: TranslatedEventKind,
}

impl TranslatedEvent {
    const fn new(conn_id: VsockConnId, kind: TranslatedEventKind) -> Self {
        Self { conn_id, kind }
    }

    const fn kind(self) -> TranslatedEventKind {
        self.kind
    }

    const fn conn_id(self) -> VsockConnId {
        self.conn_id
    }

    const fn should_surface_to_driver(self) -> bool {
        self.kind.should_surface_to_driver()
    }
}

#[derive(Debug)]
enum PollOutcome {
    NoEvent,
    DriverEvent(VsockDriverEvent),
}

impl PollOutcome {
    const fn into_option(self) -> Option<VsockDriverEvent> {
        match self {
            Self::NoEvent => None,
            Self::DriverEvent(event) => Some(event),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CreditRefreshPolicy {
    Never,
    AfterReceive,
    AfterQuery,
}

impl CreditRefreshPolicy {
    const fn should_refresh_after_recv(self, bytes_read: usize) -> bool {
        matches!(self, Self::AfterReceive) && bytes_read != 0
    }

    const fn should_refresh_after_query(self) -> bool {
        matches!(self, Self::AfterQuery)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EventRoutingDecision {
    translated: Option<TranslatedEvent>,
    refresh_credit: bool,
}

impl EventRoutingDecision {
    const fn ignore() -> Self {
        Self {
            translated: None,
            refresh_credit: false,
        }
    }

    const fn surface(translated: TranslatedEvent) -> Self {
        Self {
            translated: Some(translated),
            refresh_credit: false,
        }
    }

    const fn translated(self) -> Option<TranslatedEvent> {
        self.translated
    }

    const fn should_refresh_credit(self) -> bool {
        self.refresh_credit
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EventEndpoints {
    source: ax_driver_vsock::VsockAddr,
    destination: ax_driver_vsock::VsockAddr,
}

impl EventEndpoints {
    fn from_event(event: &VsockEvent) -> DevResult<Self> {
        let source = map_socket_addr_to_driver(event.source);
        let destination = map_socket_addr_to_driver(event.destination);
        validate_event_endpoint(&source)?;
        validate_port(destination.port)?;
        Ok(Self {
            source,
            destination,
        })
    }

    const fn source(self) -> ax_driver_vsock::VsockAddr {
        self.source
    }

    const fn destination(self) -> ax_driver_vsock::VsockAddr {
        self.destination
    }

    fn into_conn_id(self) -> VsockConnId {
        VsockConnId {
            peer_addr: self.source(),
            local_port: self.destination().port,
        }
    }
}

fn validate_port(port: u32) -> DevResult<()> {
    if port == 0 {
        return Err(DevError::InvalidParam);
    }
    Ok(())
}

fn validate_buffer_len(buf_len: usize, operation: ConnectionOperation) -> DevResult<()> {
    if operation.requires_non_empty_buffer() && buf_len == 0 {
        return Err(DevError::InvalidParam);
    }
    Ok(())
}

const fn short_circuit_empty_io(buf_len: usize) -> Option<usize> {
    if buf_len == 0 { Some(0) } else { None }
}

fn validate_peer_addr_for_operation(
    addr: &ax_driver_vsock::VsockAddr,
    _operation: ConnectionOperation,
) -> DevResult<()> {
    validate_port(addr.port)
}

fn validate_conn_id(cid: VsockConnId, operation: ConnectionOperation) -> DevResult<VsockConnId> {
    validate_peer_addr_for_operation(&cid.peer_addr, operation)?;
    validate_port(cid.local_port)?;
    Ok(cid)
}

fn validate_event_endpoint(addr: &ax_driver_vsock::VsockAddr) -> DevResult<()> {
    validate_port(addr.port)
}

fn validate_received_length_for_event(length: usize) -> DevResult<usize> {
    if length == 0 {
        return Err(DevError::InvalidParam);
    }
    Ok(length)
}

fn map_socket_addr_to_driver(addr: VsockAddr) -> ax_driver_vsock::VsockAddr {
    ax_driver_vsock::VsockAddr {
        cid: addr.cid,
        port: addr.port,
    }
}

fn map_peer_addr(addr: ax_driver_vsock::VsockAddr) -> VsockAddr {
    VsockAddr {
        cid: addr.cid,
        port: addr.port,
    }
}

fn validate_conn_id_for_operation(
    cid: VsockConnId,
    operation: ConnectionOperation,
) -> DevResult<VsockConnId> {
    let cid = validate_conn_id(cid, operation)?;
    match operation {
        ConnectionOperation::Connect
        | ConnectionOperation::Send
        | ConnectionOperation::Receive
        | ConnectionOperation::ReceiveAvailable
        | ConnectionOperation::Disconnect
        | ConnectionOperation::Abort => Ok(cid),
    }
}

fn map_conn_id_checked(
    cid: VsockConnId,
    operation: ConnectionOperation,
) -> DevResult<MappedConnId> {
    let cid = validate_conn_id_for_operation(cid, operation)?;
    Ok(MappedConnId::new(
        map_peer_addr(cid.peer_addr),
        cid.local_port,
    ))
}

fn validate_send_request(cid: VsockConnId, buf_len: usize) -> DevResult<MappedConnId> {
    validate_buffer_len(buf_len, ConnectionOperation::Send)?;
    map_conn_id_checked(cid, ConnectionOperation::Send)
}

fn validate_recv_request(cid: VsockConnId, buf_len: usize) -> DevResult<MappedConnId> {
    validate_buffer_len(buf_len, ConnectionOperation::Receive)?;
    map_conn_id_checked(cid, ConnectionOperation::Receive)
}

fn validate_credit_query(cid: VsockConnId) -> DevResult<MappedConnId> {
    map_conn_id_checked(cid, ConnectionOperation::ReceiveAvailable)
}

fn validate_disconnect_request(cid: VsockConnId) -> DevResult<MappedConnId> {
    map_conn_id_checked(cid, ConnectionOperation::Disconnect)
}

fn validate_abort_request(cid: VsockConnId) -> DevResult<MappedConnId> {
    map_conn_id_checked(cid, ConnectionOperation::Abort)
}

fn validate_connect_request(cid: VsockConnId) -> DevResult<MappedConnId> {
    map_conn_id_checked(cid, ConnectionOperation::Connect)
}

fn build_operation_request(
    cid: VsockConnId,
    operation: ConnectionOperation,
) -> DevResult<OperationRequest> {
    let conn = match operation {
        ConnectionOperation::Connect => validate_connect_request(cid)?,
        ConnectionOperation::ReceiveAvailable => validate_credit_query(cid)?,
        ConnectionOperation::Disconnect => validate_disconnect_request(cid)?,
        ConnectionOperation::Abort => validate_abort_request(cid)?,
        ConnectionOperation::Send | ConnectionOperation::Receive => {
            return Err(DevError::InvalidParam);
        }
    };
    Ok(OperationRequest::new(conn, operation))
}

fn build_buffered_operation_request(
    cid: VsockConnId,
    operation: ConnectionOperation,
    buf_len: usize,
) -> DevResult<OperationRequest> {
    let conn = match operation {
        ConnectionOperation::Send => validate_send_request(cid, buf_len)?,
        ConnectionOperation::Receive => validate_recv_request(cid, buf_len)?,
        ConnectionOperation::Connect
        | ConnectionOperation::ReceiveAvailable
        | ConnectionOperation::Disconnect
        | ConnectionOperation::Abort => return Err(DevError::InvalidParam),
    };
    Ok(OperationRequest::new(conn, operation))
}

fn prepare_listen_request(src_port: u32) -> Option<ListenRequest> {
    validate_port(src_port)
        .ok()
        .map(|_| ListenRequest::new(src_port))
}

fn credit_refresh_policy_for_operation(operation: ConnectionOperation) -> CreditRefreshPolicy {
    match operation {
        ConnectionOperation::Receive => CreditRefreshPolicy::AfterReceive,
        ConnectionOperation::ReceiveAvailable => CreditRefreshPolicy::AfterQuery,
        ConnectionOperation::Connect
        | ConnectionOperation::Send
        | ConnectionOperation::Disconnect
        | ConnectionOperation::Abort => CreditRefreshPolicy::Never,
    }
}

fn map_disconnect_reason(_reason: DisconnectReason) -> TranslatedEventKind {
    TranslatedEventKind::Disconnected
}

fn map_received_length(length: usize) -> TranslatedEventKind {
    TranslatedEventKind::Received(validate_received_length_for_event(length).unwrap_or(length))
}

fn translate_event_kind(event_type: VsockEventType) -> TranslatedEventKind {
    match event_type {
        VsockEventType::ConnectionRequest => TranslatedEventKind::ConnectionRequest,
        VsockEventType::Connected => TranslatedEventKind::Connected,
        VsockEventType::Received { length } => map_received_length(length),
        VsockEventType::Disconnected { reason } => map_disconnect_reason(reason),
        VsockEventType::CreditUpdate => TranslatedEventKind::CreditUpdate,
        _ => TranslatedEventKind::Unknown,
    }
}

/// The VirtIO socket device driver.
pub struct VirtIoSocketDev<H: Hal, T: Transport> {
    inner: InnerDev<H, T>,
}

unsafe impl<H: Hal, T: Transport> Send for VirtIoSocketDev<H, T> {}
unsafe impl<H: Hal, T: Transport> Sync for VirtIoSocketDev<H, T> {}

impl<H: Hal, T: Transport> VirtIoSocketDev<H, T> {
    /// Creates a new driver instance and initializes the device, or returns
    /// an error if any step fails.
    pub fn try_new(transport: T) -> DevResult<Self> {
        let virtio_socket = VirtIOSocket::<H, _>::new(transport).map_err(as_dev_err)?;
        Ok(Self {
            inner: InnerDev::new_with_capacity(virtio_socket, DEFAULT_RX_BUFFER_CAPACITY),
        })
    }

    fn connect_mapped(&mut self, request: OperationRequest) -> DevResult<()> {
        let conn = normalize_operation_request(request)?.conn();
        self.inner
            .connect(conn.peer_addr(), conn.local_port())
            .map_err(as_dev_err)
    }

    fn send_on_mapped(&mut self, request: OperationRequest, buf: &[u8]) -> DevResult<usize> {
        let conn = normalize_operation_request(request)?.conn();
        match self.inner.send(conn.peer_addr(), conn.local_port(), buf) {
            Ok(()) => Ok(buf.len()),
            Err(e) => Err(as_dev_err(e)),
        }
    }

    fn recv_on_mapped(&mut self, request: OperationRequest, buf: &mut [u8]) -> DevResult<usize> {
        let conn = normalize_operation_request(request)?.conn();
        let res = self
            .inner
            .recv(conn.peer_addr(), conn.local_port(), buf)
            .map_err(as_dev_err);
        self.refresh_peer_credit_after_recv(conn, &res);
        res
    }

    fn recv_available_on_mapped(&mut self, request: OperationRequest) -> DevResult<usize> {
        let conn = normalize_operation_request(request)?.conn();
        self.inner
            .recv_buffer_available_bytes(conn.peer_addr(), conn.local_port())
            .map_err(as_dev_err)
    }

    fn shutdown_mapped(&mut self, request: OperationRequest) -> DevResult<()> {
        let conn = normalize_operation_request(request)?.conn();
        self.inner
            .shutdown(conn.peer_addr(), conn.local_port())
            .map_err(as_dev_err)
    }

    fn abort_mapped(&mut self, request: OperationRequest) -> DevResult<()> {
        let conn = normalize_operation_request(request)?.conn();
        self.inner
            .force_close(conn.peer_addr(), conn.local_port())
            .map_err(as_dev_err)
    }

    fn update_peer_credit(&mut self, conn: MappedConnId) {
        let _ = self
            .inner
            .update_credit(conn.peer_addr(), conn.local_port());
    }

    fn refresh_peer_credit_after_recv(
        &mut self,
        conn: MappedConnId,
        recv_result: &DevResult<usize>,
    ) {
        let policy = credit_refresh_policy_for_operation(ConnectionOperation::Receive);
        if matches!(recv_result, Ok(bytes_read) if policy.should_refresh_after_recv(*bytes_read)) {
            self.update_peer_credit(conn);
        }
    }

    fn refresh_peer_credit_for_query(&mut self, conn: MappedConnId) {
        let policy = credit_refresh_policy_for_operation(ConnectionOperation::ReceiveAvailable);
        if policy.should_refresh_after_query() {
            self.update_peer_credit(conn);
        }
    }

    fn poll_raw_event(&mut self) -> DevResult<Option<VsockEvent>> {
        self.inner.poll().map_err(as_dev_err)
    }

    fn listen_on_port(&mut self, request: ListenRequest) {
        self.inner.listen(request.src_port())
    }

    fn poll_driver_event_once(&mut self) -> DevResult<PollOutcome> {
        match self.poll_raw_event()? {
            None => Ok(PollOutcome::NoEvent),
            Some(event) => Ok(PollOutcome::DriverEvent(convert_vsock_event(event)?)),
        }
    }
}

impl<H: Hal, T: Transport> BaseDriverOps for VirtIoSocketDev<H, T> {
    fn device_name(&self) -> &str {
        "virtio-socket"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Vsock
    }
}

#[cfg(test)]
fn map_conn_id(cid: VsockConnId) -> (VsockAddr, u32) {
    let mapped = map_conn_id_checked(cid, ConnectionOperation::Connect)
        .expect("vsock connection id should be valid");
    (mapped.peer_addr, mapped.local_port)
}

fn map_event_cid(event: &VsockEvent) -> VsockConnId {
    EventEndpoints::from_event(event)
        .map(EventEndpoints::into_conn_id)
        .unwrap_or_default()
}

fn map_driver_event(event: TranslatedEvent) -> VsockDriverEvent {
    match event.kind {
        TranslatedEventKind::ConnectionRequest => {
            VsockDriverEvent::ConnectionRequest(event.conn_id)
        }
        TranslatedEventKind::Connected => VsockDriverEvent::Connected(event.conn_id),
        TranslatedEventKind::Received(length) => VsockDriverEvent::Received(event.conn_id, length),
        TranslatedEventKind::Disconnected => VsockDriverEvent::Disconnected(event.conn_id),
        TranslatedEventKind::CreditUpdate => VsockDriverEvent::CreditUpdate(event.conn_id),
        TranslatedEventKind::Unknown => VsockDriverEvent::Unknown,
    }
}

fn translate_vsock_event(event: VsockEvent) -> TranslatedEvent {
    let conn_id = map_event_cid(&event);
    let kind = translate_event_kind(event.event_type);
    TranslatedEvent::new(conn_id, kind)
}

fn validate_translated_event(translated: TranslatedEvent) -> DevResult<TranslatedEvent> {
    validate_conn_id(translated.conn_id(), ConnectionOperation::Connect)?;
    if let TranslatedEventKind::Received(length) = translated.kind() {
        validate_received_length_for_event(length)?;
    }
    Ok(translated)
}

fn should_surface_translated_event(translated: TranslatedEvent) -> bool {
    if translated.kind().is_connection_event() {
        return true;
    }
    if translated.kind().is_data_event() {
        return true;
    }
    translated.should_surface_to_driver()
}

fn validate_mapped_conn(
    conn: MappedConnId,
    operation: ConnectionOperation,
) -> DevResult<MappedConnId> {
    let driver_conn = conn.into_driver_conn_id();
    validate_conn_id(driver_conn, operation)?;
    if operation.refreshes_credit_after_completion() {
        validate_peer_addr_for_operation(&driver_conn.peer_addr, operation)?;
    }
    let _ = operation.name();
    Ok(conn)
}

fn route_translated_event(translated: TranslatedEvent) -> EventRoutingDecision {
    if should_surface_translated_event(translated) {
        return EventRoutingDecision::surface(translated);
    }
    EventRoutingDecision::ignore()
}

fn normalize_polled_event(event: VsockEvent) -> DevResult<EventRoutingDecision> {
    let translated = validate_translated_event(translate_vsock_event(event))?;
    Ok(route_translated_event(translated))
}

impl<H: Hal, T: Transport> VsockDriverOps for VirtIoSocketDev<H, T> {
    fn guest_cid(&self) -> u64 {
        self.inner.guest_cid()
    }

    fn listen(&mut self, src_port: u32) {
        if let Some(request) = prepare_listen_request(src_port)
            && should_listen_on_port(request)
            && let Some(request) = normalize_listen_request(request)
        {
            self.listen_on_port(request);
        }
    }

    fn connect(&mut self, cid: VsockConnId) -> DevResult<()> {
        let request = build_operation_request(cid, ConnectionOperation::Connect)?;
        self.connect_mapped(request)
    }

    fn send(&mut self, cid: VsockConnId, buf: &[u8]) -> DevResult<usize> {
        if let Some(result) = short_circuit_empty_io(buf.len()) {
            return Ok(result);
        }
        let request = build_buffered_operation_request(cid, ConnectionOperation::Send, buf.len())?;
        self.send_on_mapped(request, buf)
    }

    fn recv(&mut self, cid: VsockConnId, buf: &mut [u8]) -> DevResult<usize> {
        if let Some(result) = short_circuit_empty_io(buf.len()) {
            return Ok(result);
        }
        let request =
            build_buffered_operation_request(cid, ConnectionOperation::Receive, buf.len())?;
        self.recv_on_mapped(request, buf)
    }

    fn recv_avail(&mut self, cid: VsockConnId) -> DevResult<usize> {
        let request = build_operation_request(cid, ConnectionOperation::ReceiveAvailable)?;
        let conn = request.conn();
        let available = self.recv_available_on_mapped(request)?;
        self.refresh_peer_credit_for_query(conn);
        Ok(available)
    }

    fn disconnect(&mut self, cid: VsockConnId) -> DevResult<()> {
        let request = build_operation_request(cid, ConnectionOperation::Disconnect)?;
        self.shutdown_mapped(request)
    }

    fn abort(&mut self, cid: VsockConnId) -> DevResult<()> {
        let request = build_operation_request(cid, ConnectionOperation::Abort)?;
        self.abort_mapped(request)
    }

    fn poll_event(&mut self) -> DevResult<Option<VsockDriverEvent>> {
        Ok(self.poll_driver_event_once()?.into_option())
    }
}

fn convert_vsock_event(event: VsockEvent) -> DevResult<VsockDriverEvent> {
    let decision = normalize_polled_event(event)?;
    let _ = decision.should_refresh_credit();
    if let Some(translated) = decision.translated() {
        return Ok(map_driver_event(translated));
    }
    Ok(VsockDriverEvent::Unknown)
}

#[cfg(test)]
mod tests {
    use ax_driver_vsock::{VsockAddr as DriverVsockAddr, VsockConnId, VsockDriverEvent};
    use virtio_drivers::device::socket::{DisconnectReason, VsockAddr, VsockEvent, VsockEventType};

    use super::{
        ConnectionOperation, convert_vsock_event, map_conn_id, map_event_cid,
        short_circuit_empty_io, validate_conn_id_for_operation, validate_event_endpoint,
    };

    fn sample_conn_id() -> VsockConnId {
        VsockConnId {
            peer_addr: DriverVsockAddr {
                cid: 52,
                port: 2048,
            },
            local_port: 4096,
        }
    }

    fn sample_event(event_type: VsockEventType) -> VsockEvent {
        let mut event: VsockEvent = unsafe { core::mem::zeroed() };
        event.source = VsockAddr {
            cid: 33,
            port: 1025,
        };
        event.destination = VsockAddr {
            cid: 44,
            port: 2049,
        };
        event.event_type = event_type;
        event
    }

    #[test]
    fn map_conn_id_preserves_peer_and_local_port() {
        let conn_id = sample_conn_id();
        let (peer_addr, local_port) = map_conn_id(conn_id);
        assert_eq!(peer_addr.cid, conn_id.peer_addr.cid as _);
        assert_eq!(peer_addr.port, conn_id.peer_addr.port);
        assert_eq!(local_port, conn_id.local_port);
    }

    #[test]
    fn map_event_cid_uses_event_endpoints() {
        let event = sample_event(VsockEventType::Connected);
        let conn_id = map_event_cid(&event);
        assert_eq!(conn_id.peer_addr.cid, event.source.cid as _);
        assert_eq!(conn_id.peer_addr.port, event.source.port);
        assert_eq!(conn_id.local_port, event.destination.port);
    }

    #[test]
    fn convert_vsock_event_maps_connection_request() {
        let event = sample_event(VsockEventType::ConnectionRequest);
        let mapped = convert_vsock_event(event).unwrap();
        assert!(matches!(mapped, VsockDriverEvent::ConnectionRequest(_)));
    }

    #[test]
    fn convert_vsock_event_maps_connected() {
        let event = sample_event(VsockEventType::Connected);
        let mapped = convert_vsock_event(event).unwrap();
        assert!(matches!(mapped, VsockDriverEvent::Connected(_)));
    }

    #[test]
    fn convert_vsock_event_maps_received_length() {
        let event = sample_event(VsockEventType::Received { length: 128 });
        let mapped = convert_vsock_event(event).unwrap();
        assert!(matches!(mapped, VsockDriverEvent::Received(_, 128)));
    }

    #[test]
    fn convert_vsock_event_maps_disconnected() {
        let event = sample_event(VsockEventType::Disconnected {
            reason: DisconnectReason::Shutdown,
        });
        let mapped = convert_vsock_event(event).unwrap();
        assert!(matches!(mapped, VsockDriverEvent::Disconnected(_)));
    }

    #[test]
    fn convert_vsock_event_maps_credit_update() {
        let event = sample_event(VsockEventType::CreditUpdate);
        let mapped = convert_vsock_event(event).unwrap();
        assert!(matches!(mapped, VsockDriverEvent::CreditUpdate(_)));
    }

    #[test]
    fn convert_vsock_event_maps_credit_request_to_unknown() {
        let event = sample_event(VsockEventType::CreditRequest);
        let mapped = convert_vsock_event(event).unwrap();
        assert!(matches!(mapped, VsockDriverEvent::Unknown));
    }

    #[test]
    fn short_circuit_empty_io_returns_zero_for_empty_buffer() {
        assert_eq!(short_circuit_empty_io(0), Some(0));
    }

    #[test]
    fn short_circuit_empty_io_skips_non_empty_buffer() {
        assert_eq!(short_circuit_empty_io(8), None);
    }

    #[test]
    fn connect_validation_allows_hypervisor_cid() {
        let conn = VsockConnId {
            peer_addr: DriverVsockAddr { cid: 0, port: 1025 },
            local_port: 2048,
        };

        assert!(validate_conn_id_for_operation(conn, ConnectionOperation::Connect).is_ok());
    }

    #[test]
    fn send_validation_allows_hypervisor_cid() {
        let conn = VsockConnId {
            peer_addr: DriverVsockAddr { cid: 0, port: 1025 },
            local_port: 2048,
        };

        assert!(validate_conn_id_for_operation(conn, ConnectionOperation::Send).is_ok());
    }

    #[test]
    fn event_endpoint_allows_hypervisor_cid() {
        let endpoint = DriverVsockAddr { cid: 0, port: 1025 };

        assert!(validate_event_endpoint(&endpoint).is_ok());
    }

    #[test]
    fn peer_port_zero_remains_invalid() {
        let conn = VsockConnId {
            peer_addr: DriverVsockAddr { cid: 0, port: 0 },
            local_port: 2048,
        };

        assert!(validate_conn_id_for_operation(conn, ConnectionOperation::Connect).is_err());
    }
}

//! Board-hosted browser transports for the Axvisor and guest consoles.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::{
    string::String,
    sync::{Arc, OnceLock},
    time::Duration,
};

use anyhow::{Context, Result};
use ax_std::os::arceos::sync::NoPreemptMutex;
use axvisor::console_mux::HostOutputQueue;
use axvm::VMId;

mod layout;

mod delivery;

use delivery::{BlockingSignal, DeliveryFrame, DeliveryQueue};
use layout::{ConsoleLane, Endpoint, plan_endpoints};

const CONSOLE_LANE_COUNT: usize = ConsoleLane::COUNT;
const OUTPUT_QUEUE_CAPACITY: usize = 64 * 1024;
const OUTPUT_BATCH_CAPACITY: usize = 512;
const OUTPUT_FRAME_TARGET_CAPACITY: usize = 4096;
const OUTPUT_DELIVERY_QUEUE_CAPACITY: usize = 64 * 1024;
const OUTPUT_DELIVERY_BATCH_CAPACITY: usize = 4096;
const OUTPUT_COALESCE_WINDOW: Duration = Duration::from_millis(10);
const MANAGEMENT_LINE_CAPACITY: usize = 256;

static OUTPUT_HUB: NetworkOutputHub = NetworkOutputHub::new();
static ENDPOINTS: OnceLock<Vec<Endpoint>> = OnceLock::new();

struct NetworkOutputHub {
    lanes: [NetworkOutputLane; CONSOLE_LANE_COUNT],
}

struct NetworkOutputLane {
    queue: NoPreemptMutex<HostOutputQueue<OUTPUT_QUEUE_CAPACITY>>,
    connected: AtomicBool,
    session: AtomicUsize,
    ready: BlockingSignal,
}

struct BrowserOutputDelivery {
    queue: NoPreemptMutex<DeliveryQueue<OUTPUT_DELIVERY_QUEUE_CAPACITY>>,
    closed: AtomicBool,
    ready: BlockingSignal,
}

impl NetworkOutputHub {
    const fn new() -> Self {
        Self {
            lanes: [
                NetworkOutputLane::new(),
                NetworkOutputLane::new(),
                NetworkOutputLane::new(),
                NetworkOutputLane::new(),
            ],
        }
    }

    fn submit(&self, lane: ConsoleLane, bytes: &[u8]) {
        self.lanes[lane.index()].submit(bytes);
    }

    fn is_connected(&self, lane: ConsoleLane) -> bool {
        self.lanes[lane.index()].connected.load(Ordering::Acquire)
    }

    fn begin_session(&self, lane: ConsoleLane) -> Option<usize> {
        self.lanes[lane.index()].begin_session()
    }

    fn end_session(&self, lane: ConsoleLane, session: usize) {
        self.lanes[lane.index()].end_session(session);
    }

    fn receive(&self, lane: ConsoleLane, session: usize) -> Option<NetworkOutputBatch> {
        self.lanes[lane.index()].receive(session)
    }

    fn take_batch(&self, lane: ConsoleLane, session: usize) -> Option<NetworkOutputBatch> {
        self.lanes[lane.index()].take_batch(session)
    }
}

impl NetworkOutputLane {
    const fn new() -> Self {
        Self {
            queue: NoPreemptMutex::new(HostOutputQueue::new()),
            connected: AtomicBool::new(false),
            session: AtomicUsize::new(0),
            ready: BlockingSignal::new(),
        }
    }

    fn submit(&self, bytes: &[u8]) {
        if bytes.is_empty() || !self.connected.load(Ordering::Acquire) {
            return;
        }
        let submitted = {
            let mut queue = self.queue.lock();
            if !self.connected.load(Ordering::Acquire) {
                false
            } else {
                queue.enqueue(bytes);
                true
            }
        };
        if submitted {
            self.ready.notify_irq();
        }
    }

    fn begin_session(&self) -> Option<usize> {
        let mut queue = self.queue.lock();
        self.connected
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        *queue = HostOutputQueue::new();
        self.ready.drain();
        Some(self.session.fetch_add(1, Ordering::AcqRel).wrapping_add(1))
    }

    fn end_session(&self, session: usize) {
        let mut queue = self.queue.lock();
        if self.session.load(Ordering::Acquire) == session {
            self.connected.store(false, Ordering::Release);
            *queue = HostOutputQueue::new();
            drop(queue);
            self.ready.notify();
        }
    }

    fn take_batch(&self, session: usize) -> Option<NetworkOutputBatch> {
        let mut queue = self.queue.lock();
        if !self.connected.load(Ordering::Acquire)
            || self.session.load(Ordering::Acquire) != session
        {
            return None;
        }
        let mut batch = NetworkOutputBatch::new();
        batch.dropped_bytes = queue.take_dropped_bytes();
        batch.len = queue.dequeue(&mut batch.bytes);
        (!batch.is_empty()).then_some(batch)
    }

    fn receive(&self, session: usize) -> Option<NetworkOutputBatch> {
        loop {
            if let Some(batch) = self.take_batch(session) {
                return Some(batch);
            }
            if !self.connected.load(Ordering::Acquire)
                || self.session.load(Ordering::Acquire) != session
            {
                return None;
            }
            self.ready.wait();
        }
    }
}

impl BrowserOutputDelivery {
    const fn new() -> Self {
        Self {
            queue: NoPreemptMutex::new(DeliveryQueue::new()),
            closed: AtomicBool::new(false),
            ready: BlockingSignal::new(),
        }
    }

    fn enqueue(&self, bytes: &[u8]) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let submitted = {
            let mut queue = self.queue.lock();
            if self.closed.load(Ordering::Acquire) {
                false
            } else {
                queue.enqueue(bytes);
                true
            }
        };
        if submitted {
            self.ready.notify();
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.ready.notify();
    }

    fn receive(&self) -> Option<Vec<u8>> {
        loop {
            let mut bytes = [0; OUTPUT_DELIVERY_BATCH_CAPACITY];
            let (len, dropped_bytes) = self.queue.lock().dequeue(&mut bytes);
            if len != 0 || dropped_bytes != 0 {
                let mut frame = DeliveryFrame::with_capacity(len + 96);
                frame.append(&bytes[..len], dropped_bytes);
                return Some(frame.into_bytes());
            }
            if self.closed.load(Ordering::Acquire) {
                return None;
            }
            self.ready.wait();
        }
    }
}

struct NetworkOutputBatch {
    bytes: [u8; OUTPUT_BATCH_CAPACITY],
    len: usize,
    dropped_bytes: usize,
}

impl NetworkOutputBatch {
    const fn new() -> Self {
        Self {
            bytes: [0; OUTPUT_BATCH_CAPACITY],
            len: 0,
            dropped_bytes: 0,
        }
    }

    const fn is_empty(&self) -> bool {
        self.len == 0 && self.dropped_bytes == 0
    }
}

struct ActiveSession {
    lane: ConsoleLane,
    session: usize,
}

impl ActiveSession {
    fn install(lane: ConsoleLane) -> Result<Self> {
        let session = OUTPUT_HUB.begin_session(lane).with_context(|| {
            format!("{} console already has an active session", lane_name(lane))
        })?;
        Ok(Self { lane, session })
    }
}

impl Drop for ActiveSession {
    fn drop(&mut self) {
        OUTPUT_HUB.end_session(self.lane, self.session);
    }
}

/// Captures the immutable console layout after the startup VMs are registered.
pub(crate) fn start() -> Result<()> {
    ENDPOINTS
        .set(build_startup_endpoints())
        .map_err(|_| anyhow::anyhow!("browser console endpoints were already initialized"))
}

fn build_startup_endpoints() -> Vec<Endpoint> {
    let guests = crate::manager::AxvmManager::vm_list()
        .into_iter()
        .map(|vm| (vm.id(), vm.name()))
        .collect();
    plan_endpoints(guests)
}

fn endpoints() -> &'static [Endpoint] {
    ENDPOINTS.get().map(Vec::as_slice).unwrap_or(&[])
}

fn endpoint_for_lane(lane: ConsoleLane) -> Option<&'static Endpoint> {
    endpoints().iter().find(|endpoint| endpoint.lane == lane)
}

fn endpoint_for_route(route: &str) -> Option<&'static Endpoint> {
    endpoints().iter().find(|endpoint| endpoint.route == route)
}

fn lane_name(lane: ConsoleLane) -> String {
    endpoint_for_lane(lane)
        .map(|endpoint| endpoint.display_name.clone())
        .unwrap_or_else(|| format!("console lane {}", lane.index()))
}

/// Returns whether the startup snapshot exposed this browser route.
pub(crate) fn has_console_route(route: &str) -> bool {
    endpoint_for_route(route).is_some()
}

/// Browser-visible console descriptors for the startup VM snapshot.
pub(crate) fn console_descriptions() -> Vec<ConsoleDescription> {
    endpoints()
        .iter()
        .map(|endpoint| ConsoleDescription {
            route: endpoint.route.clone(),
            display_name: endpoint.display_name.clone(),
        })
        .collect()
}

/// One console entry returned to the embedded browser page.
pub(crate) struct ConsoleDescription {
    pub(crate) route: String,
    pub(crate) display_name: String,
}

/// Copies Axvisor shell bytes into its fixed browser queue.
pub(crate) fn submit_management_output(bytes: &[u8]) {
    OUTPUT_HUB.submit(ConsoleLane::MANAGEMENT, bytes);
}

/// Returns whether one guest currently has an attached browser session.
pub(crate) fn guest_output_connected(vm_id: VMId) -> bool {
    endpoints()
        .iter()
        .find(|endpoint| endpoint.vm_id == Some(vm_id))
        .is_some_and(|endpoint| OUTPUT_HUB.is_connected(endpoint.lane))
}

/// Copies current guest output into its VM-specific fixed browser queue.
pub(crate) fn submit_guest_output(vm_id: VMId, bytes: &[u8]) {
    let Some(lane) = endpoints()
        .iter()
        .find(|endpoint| endpoint.vm_id == Some(vm_id))
        .map(|endpoint| endpoint.lane)
    else {
        return;
    };
    OUTPUT_HUB.submit(lane, bytes);
}

/// Opens one in-process browser transport on an existing console lane.
pub(crate) fn open_browser_console(
    route: &str,
) -> Result<(BrowserConsoleInput, BrowserConsoleOutput)> {
    let endpoint = endpoint_for_route(route)
        .cloned()
        .with_context(|| format!("unknown console endpoint `{route}`"))?;
    let active_session = ActiveSession::install(endpoint.lane)?;
    let lane = active_session.lane;
    let session = active_session.session;
    let delivery = start_output_dispatcher(lane, session)?;
    Ok((
        BrowserConsoleInput {
            endpoint,
            editor: ManagementLineEditor::new(),
            _active_session: active_session,
        },
        BrowserConsoleOutput { delivery },
    ))
}

fn start_output_dispatcher(
    lane: ConsoleLane,
    session: usize,
) -> Result<Arc<BrowserOutputDelivery>> {
    let delivery = Arc::new(BrowserOutputDelivery::new());
    let dispatcher_delivery = Arc::clone(&delivery);
    let task_name = format!("{}-browser-console-dispatcher", lane_name(lane));
    std::thread::Builder::new()
        .name(task_name.clone())
        .spawn(move || run_output_dispatcher(lane, session, dispatcher_delivery))
        .with_context(|| format!("failed to start {task_name}"))?;
    Ok(delivery)
}

fn run_output_dispatcher(lane: ConsoleLane, session: usize, delivery: Arc<BrowserOutputDelivery>) {
    while let Some(frame) = receive_output_frame(lane, session) {
        delivery.enqueue(&frame);
    }
    delivery.close();
}

fn receive_output_frame(lane: ConsoleLane, session: usize) -> Option<Vec<u8>> {
    let first = OUTPUT_HUB.receive(lane, session)?;
    let mut frame = DeliveryFrame::with_capacity(OUTPUT_FRAME_TARGET_CAPACITY);
    frame.append(&first.bytes[..first.len], first.dropped_bytes);

    // UART backends commonly submit one byte at a time. Coalesce for one
    // bounded interval in the dispatcher so the HTTP reactor handles frames,
    // not individual device writes.
    std::thread::sleep(OUTPUT_COALESCE_WINDOW);
    while frame.len() < OUTPUT_FRAME_TARGET_CAPACITY {
        let Some(batch) = OUTPUT_HUB.take_batch(lane, session) else {
            break;
        };
        frame.append(&batch.bytes[..batch.len], batch.dropped_bytes);
    }
    Some(frame.into_bytes())
}

/// Input half of a board-hosted browser console session.
pub(crate) struct BrowserConsoleInput {
    endpoint: Endpoint,
    editor: ManagementLineEditor,
    _active_session: ActiveSession,
}

impl BrowserConsoleInput {
    /// Returns the session greeting sent before queued console output.
    pub(crate) fn greeting(&self) -> String {
        if let Some(vm_id) = self.endpoint.vm_id {
            format!("[Axvisor] browser console attached to VM {vm_id}\r\n")
        } else {
            format!(
                "Welcome to AxVisor Browser Shell!\r\nType 'help' for commands.\r\n{}",
                crate::shell::network_prompt()
            )
        }
    }

    /// Routes browser bytes to the selected shell and reports whether it stays open.
    pub(crate) fn route(&mut self, bytes: &[u8]) -> bool {
        if let Some(vm_id) = self.endpoint.vm_id {
            crate::guest_console::route_network_input(vm_id, bytes);
            true
        } else {
            self.editor.process(bytes)
        }
    }
}

/// Blocking output half fed by the lane's fixed-capacity delivery queue.
pub(crate) struct BrowserConsoleOutput {
    delivery: Arc<BrowserOutputDelivery>,
}

impl BrowserConsoleOutput {
    /// Waits for one coalesced frame or session closure.
    pub(crate) fn receive(&mut self) -> Option<Vec<u8>> {
        self.delivery.receive()
    }
}

struct ManagementLineEditor {
    line: [u8; MANAGEMENT_LINE_CAPACITY],
    len: usize,
    previous_was_cr: bool,
}

impl ManagementLineEditor {
    const fn new() -> Self {
        Self {
            line: [0; MANAGEMENT_LINE_CAPACITY],
            len: 0,
            previous_was_cr: false,
        }
    }

    fn process(&mut self, bytes: &[u8]) -> bool {
        for &byte in bytes {
            if byte == b'\n' && self.previous_was_cr {
                self.previous_was_cr = false;
                continue;
            }
            self.previous_was_cr = byte == b'\r';
            match byte {
                b'\r' | b'\n' => {
                    submit_management_output(b"\r\n");
                    let command = String::from_utf8_lossy(&self.line[..self.len]);
                    self.len = 0;
                    if !crate::shell::run_network_command(&command) {
                        submit_management_output(b"Goodbye!\r\n");
                        return false;
                    }
                    submit_management_output(crate::shell::network_prompt().as_bytes());
                }
                b'\x08' | b'\x7f' if self.len != 0 => {
                    self.len -= 1;
                    submit_management_output(b"\x08 \x08");
                }
                0x20..=0x7e if self.len < self.line.len() => {
                    self.line[self.len] = byte;
                    self.len += 1;
                    submit_management_output(&[byte]);
                }
                0x20..=0x7e => submit_management_output(b"\x07"),
                _ => {}
            }
        }
        true
    }
}

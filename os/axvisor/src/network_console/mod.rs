//! Board-hosted browser transports for the Axvisor and guest consoles.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::{string::String, sync::OnceLock};

use anyhow::{Context, Result};
use ax_std::os::arceos::{
    modules::ax_task::{AxCpuMask, IrqNotify, set_current_affinity},
    sync::NoPreemptMutex,
};
use axvisor::console_mux::HostOutputQueue;
use axvm::VMId;

mod layout;

use layout::{ConsoleLane, Endpoint, plan_endpoints};

const CONSOLE_LANE_COUNT: usize = ConsoleLane::COUNT;
const OUTPUT_QUEUE_CAPACITY: usize = 64 * 1024;
const OUTPUT_BATCH_CAPACITY: usize = 512;
const MANAGEMENT_LINE_CAPACITY: usize = 256;
const MANAGEMENT_CPU_ID: usize = 0;

static OUTPUT_HUB: NetworkOutputHub = NetworkOutputHub::new();
static ENDPOINTS: OnceLock<Vec<Endpoint>> = OnceLock::new();

struct NetworkOutputHub {
    queues: NoPreemptMutex<[HostOutputQueue<OUTPUT_QUEUE_CAPACITY>; CONSOLE_LANE_COUNT]>,
    connected: [AtomicBool; CONSOLE_LANE_COUNT],
    sessions: [AtomicUsize; CONSOLE_LANE_COUNT],
    ready: [IrqNotify; CONSOLE_LANE_COUNT],
}

impl NetworkOutputHub {
    const fn new() -> Self {
        Self {
            queues: NoPreemptMutex::new([
                HostOutputQueue::new(),
                HostOutputQueue::new(),
                HostOutputQueue::new(),
                HostOutputQueue::new(),
            ]),
            connected: [
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
            ],
            sessions: [
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
            ],
            ready: [
                IrqNotify::new(),
                IrqNotify::new(),
                IrqNotify::new(),
                IrqNotify::new(),
            ],
        }
    }

    fn submit(&self, lane: ConsoleLane, bytes: &[u8]) {
        if bytes.is_empty() || !self.connected[lane.index()].load(Ordering::Acquire) {
            return;
        }
        let submitted = {
            let mut queues = self.queues.lock();
            if !self.connected[lane.index()].load(Ordering::Acquire) {
                false
            } else {
                queues[lane.index()].enqueue(bytes);
                true
            }
        };
        if submitted {
            self.ready[lane.index()].notify_irq();
        }
    }

    fn is_connected(&self, lane: ConsoleLane) -> bool {
        self.connected[lane.index()].load(Ordering::Acquire)
    }

    fn begin_session(&self, lane: ConsoleLane) -> Option<usize> {
        let mut queues = self.queues.lock();
        self.connected[lane.index()]
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        queues[lane.index()] = HostOutputQueue::new();
        self.ready[lane.index()].drain();
        Some(
            self.sessions[lane.index()]
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1),
        )
    }

    fn end_session(&self, lane: ConsoleLane, session: usize) {
        let mut queues = self.queues.lock();
        if self.sessions[lane.index()].load(Ordering::Acquire) == session {
            self.connected[lane.index()].store(false, Ordering::Release);
            queues[lane.index()] = HostOutputQueue::new();
            drop(queues);
            self.ready[lane.index()].notify();
        }
    }

    fn take_batch(&self, lane: ConsoleLane, session: usize) -> Option<NetworkOutputBatch> {
        let mut queues = self.queues.lock();
        if !self.connected[lane.index()].load(Ordering::Acquire)
            || self.sessions[lane.index()].load(Ordering::Acquire) != session
        {
            return None;
        }
        let mut batch = NetworkOutputBatch::new();
        batch.dropped_bytes = queues[lane.index()].take_dropped_bytes();
        batch.len = queues[lane.index()].dequeue(&mut batch.bytes);
        (!batch.is_empty()).then_some(batch)
    }

    fn receive(&self, lane: ConsoleLane, session: usize) -> Option<NetworkOutputBatch> {
        loop {
            if let Some(batch) = self.take_batch(lane, session) {
                return Some(batch);
            }
            if !self.connected[lane.index()].load(Ordering::Acquire)
                || self.sessions[lane.index()].load(Ordering::Acquire) != session
            {
                return None;
            }
            self.ready[lane.index()].wait();
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

/// Pins one web-console service task to the management CPU.
pub(crate) fn pin_current_task() {
    assert!(
        set_current_affinity(AxCpuMask::one_shot(MANAGEMENT_CPU_ID)),
        "web console management CPU affinity must be valid"
    );
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
    Ok((
        BrowserConsoleInput {
            endpoint,
            editor: ManagementLineEditor::new(),
            _active_session: active_session,
        },
        BrowserConsoleOutput { lane, session },
    ))
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

/// Blocking output half woken only by output or session closure.
pub(crate) struct BrowserConsoleOutput {
    lane: ConsoleLane,
    session: usize,
}

impl BrowserConsoleOutput {
    /// Waits for one bounded console batch or the browser session to close.
    pub(crate) fn receive(&mut self) -> Option<Vec<u8>> {
        let batch = OUTPUT_HUB.receive(self.lane, self.session)?;
        let mut output = Vec::with_capacity(batch.len + 96);
        if batch.dropped_bytes != 0 {
            output.extend_from_slice(
                format!(
                    "\r\n[Axvisor browser console dropped {} queued bytes]\r\n",
                    batch.dropped_bytes
                )
                .as_bytes(),
            );
        }
        output.extend_from_slice(&batch.bytes[..batch.len]);
        Some(output)
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

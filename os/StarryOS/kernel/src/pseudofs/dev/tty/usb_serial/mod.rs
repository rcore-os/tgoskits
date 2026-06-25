mod backend;
mod descriptors;

use alloc::{collections::VecDeque, string::ToString, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_sync::Mutex;
use axpoll::{IoEvents, PollSet};
use spin::LazyLock;

use self::backend::{UsbSerialPortInfo, find_usb_serial_port};
use super::{
    Tty,
    terminal::{
        Terminal,
        ldisc::{ProcessMode, TtyConfig, TtyRead, TtyWrite},
        termios::Termios2,
    },
};
use crate::pseudofs::usbfs::{self, UsbDeviceHandle};

pub type UsbSerialTtyDriver = Tty<UsbSerialReader, UsbSerialWriter>;

// The devfs entry is static for now: /dev/ttyUSB0 is always present and lazily
// attaches to the first supported USB serial adapter when the tty is opened.
const USB_SERIAL_PORTS: usize = 1;
const USB_SERIAL_DEFAULT_BAUDRATE: u32 = 115_200;
const USB_SERIAL_RX_CHUNK: usize = 64;
const USB_SERIAL_TX_CHUNK: usize = 256;
const USB_SERIAL_RX_QUEUE_CAP: usize = 4096;
const USB_SERIAL_TX_QUEUE_CAP: usize = 4096;

static USB_SERIAL_TTYS: LazyLock<Vec<Arc<UsbSerialTtyDriver>>> = LazyLock::new(|| {
    (0..USB_SERIAL_PORTS)
        .map(new_usb_serial_tty)
        .collect::<Vec<_>>()
});

struct UsbSerialSession {
    handle: UsbDeviceHandle,
    port: UsbSerialPortInfo,
}

struct UsbSerialBackendState {
    index: usize,
    // Owns the usbfs lease and claimed interface. Keeping this behind the tty
    // backend, instead of per open file, matches the current static devfs node
    // model and lets the RX/TX workers share one hardware session.
    session: Mutex<Option<Arc<UsbSerialSession>>>,
    baudrate: AtomicU32,
    started: AtomicBool,
    session_closing: AtomicBool,
    rx_worker_started: AtomicBool,
    tx_worker_started: AtomicBool,
    rx_queue: SpinNoIrq<VecDeque<u8>>,
    tx_queue: SpinNoIrq<VecDeque<u8>>,
    dropped_rx: AtomicUsize,
    input_source: Arc<PollSet>,
    output_source: Arc<PollSet>,
    output_lock: Mutex<()>,
}

#[derive(Clone)]
pub struct UsbSerialReader {
    backend: Arc<UsbSerialBackendState>,
}

#[derive(Clone)]
pub struct UsbSerialWriter {
    backend: Arc<UsbSerialBackendState>,
}

pub fn usb_serial_tty(index: usize) -> Option<Arc<UsbSerialTtyDriver>> {
    USB_SERIAL_TTYS.get(index).cloned()
}

impl UsbSerialTtyDriver {
    pub fn usb_serial_number(&self) -> usize {
        self.writer.backend.index
    }
}

fn new_usb_serial_tty(index: usize) -> Arc<UsbSerialTtyDriver> {
    let backend = Arc::new(UsbSerialBackendState {
        index,
        session: Mutex::new(None),
        baudrate: AtomicU32::new(USB_SERIAL_DEFAULT_BAUDRATE),
        started: AtomicBool::new(false),
        session_closing: AtomicBool::new(false),
        rx_worker_started: AtomicBool::new(false),
        tx_worker_started: AtomicBool::new(false),
        rx_queue: SpinNoIrq::new(VecDeque::new()),
        tx_queue: SpinNoIrq::new(VecDeque::new()),
        dropped_rx: AtomicUsize::new(0),
        input_source: Arc::new(PollSet::new()),
        output_source: Arc::new(PollSet::new()),
        output_lock: Mutex::new(()),
    });

    let terminal = Arc::new(Terminal::default());
    *terminal.termios.lock() = Arc::new(Termios2::default_b115200());
    Tty::new(
        terminal,
        TtyConfig {
            reader: UsbSerialReader {
                backend: backend.clone(),
            },
            writer: UsbSerialWriter {
                backend: backend.clone(),
            },
            process_mode: ProcessMode::InterruptDriven {
                input: backend.input_source.clone(),
                output: Some(backend.output_source.clone()),
            },
        },
    )
}

impl UsbSerialBackendState {
    fn ensure_started(self: &Arc<Self>) -> AxResult<()> {
        self.ensure_session()?;
        self.started.store(true, Ordering::Release);
        self.start_rx_worker();
        Ok(())
    }

    // Attach lazily so the tty can exist before the adapter is plugged in. A
    // closing session rejects new opens/writes until the RX worker finishes
    // deferred teardown.
    fn ensure_session(&self) -> AxResult<Arc<UsbSerialSession>> {
        if self.session_closing.load(Ordering::Acquire) {
            return Err(AxError::ResourceBusy);
        }

        let mut session = self.session.lock();
        if let Some(session) = session.as_ref() {
            return Ok(session.clone());
        }

        if self.session_closing.load(Ordering::Acquire) {
            return Err(AxError::ResourceBusy);
        }

        let port = find_usb_serial_port(self.index).ok_or(AxError::NoSuchDevice)?;
        let handle = usbfs::acquire_usb_device(port.bus_num, port.device_num)?;
        handle.claim_interface(port.interface, 0)?;
        let baudrate = self.baudrate.load(Ordering::Acquire);
        if let Err(err) = port.backend.init(&handle, &port, baudrate) {
            let _ = handle.release_interface(port.interface);
            return Err(err);
        }
        info!(
            "usb-serial: ttyUSB{} attached to {} device {}:{} iface {} in={:#04x} out={:#04x}",
            self.index,
            port.backend.name(),
            port.bus_num,
            port.device_num,
            port.interface,
            port.bulk_in,
            port.bulk_out
        );
        let new_session = Arc::new(UsbSerialSession { handle, port });
        *session = Some(new_session.clone());
        Ok(new_session)
    }

    fn set_baudrate(self: &Arc<Self>, baudrate: u32) -> AxResult<()> {
        if baudrate == 0 {
            return Ok(());
        }
        let old = self.baudrate.swap(baudrate, Ordering::AcqRel);
        if old == baudrate {
            return Ok(());
        }
        let session = self.ensure_session()?;
        if let Err(err) = session
            .port
            .backend
            .set_baud(&session.handle, &session.port, baudrate)
        {
            self.request_session_teardown(Some(&session));
            self.baudrate.store(old, Ordering::Release);
            return Err(err);
        }
        Ok(())
    }

    fn write_bytes(self: &Arc<Self>, buf: &[u8]) -> AxResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.ensure_started()?;
        let _guard = self.output_lock.lock();
        // write() and try_write() share tx_queue so terminal echo bytes and
        // application output leave through one ordered stream.
        let mut queued = 0;
        while queued < buf.len() {
            let accepted = self.push_tx_bytes(&buf[queued..]);
            queued += accepted;
            if queued < buf.len() {
                self.drain_tx_queue_locked()?;
            }
        }
        self.drain_tx_queue_locked()?;
        Ok(queued)
    }

    fn try_queue_bytes(self: &Arc<Self>, buf: &[u8]) -> usize {
        if buf.is_empty()
            || !self.started.load(Ordering::Acquire)
            || self.session_closing.load(Ordering::Acquire)
        {
            return 0;
        }
        // Called from line-discipline echo paths, so it must not block on USB
        // I/O or create a new session. It only queues bytes and nudges TX.
        let queued = self.push_tx_bytes(buf);
        if queued > 0 {
            self.start_tx_worker();
        }
        queued
    }

    fn push_tx_bytes(&self, buf: &[u8]) -> usize {
        let mut queue = self.tx_queue.lock();
        let space = USB_SERIAL_TX_QUEUE_CAP.saturating_sub(queue.len());
        let queued = buf.len().min(space);
        queue.extend(buf[..queued].iter().copied());
        queued
    }

    fn pop_tx_chunk(&self) -> Vec<u8> {
        let mut queue = self.tx_queue.lock();
        let len = queue.len().min(USB_SERIAL_TX_CHUNK);
        let mut chunk = Vec::with_capacity(len);
        for _ in 0..len {
            if let Some(byte) = queue.pop_front() {
                chunk.push(byte);
            }
        }
        chunk
    }

    fn requeue_tx_front(&self, bytes: &[u8]) {
        let mut queue = self.tx_queue.lock();
        for &byte in bytes.iter().rev() {
            queue.push_front(byte);
        }
    }

    fn clear_tx_queue(&self) {
        self.tx_queue.lock().clear();
        unsafe { self.output_source.wake(IoEvents::OUT) };
    }

    fn drain_tx_queue_locked(self: &Arc<Self>) -> AxResult<()> {
        loop {
            let chunk = self.pop_tx_chunk();
            if chunk.is_empty() {
                unsafe { self.output_source.wake(IoEvents::OUT) };
                return Ok(());
            }

            let session = self.ensure_session()?;
            let mut offset = 0;
            while offset < chunk.len() {
                let actual = match session
                    .handle
                    .bulk_out(session.port.bulk_out, &chunk[offset..])
                {
                    Ok(actual) => actual,
                    Err(err) => {
                        // Requeue before teardown so the TX queue remains
                        // ordered if the failed session is no longer current.
                        self.requeue_tx_front(&chunk[offset..]);
                        self.request_session_teardown(Some(&session));
                        self.clear_tx_queue();
                        return Err(err);
                    }
                };

                if actual == 0 {
                    self.request_session_teardown(Some(&session));
                    self.clear_tx_queue();
                    return Err(AxError::WriteZero);
                }

                offset += actual.min(chunk.len() - offset);
            }
            unsafe { self.output_source.wake(IoEvents::OUT) };
        }
    }

    fn drain_rx(&self, buf: &mut [u8]) -> usize {
        let mut queue = self.rx_queue.lock();
        let count = buf.len().min(queue.len());
        for slot in buf.iter_mut().take(count) {
            *slot = queue
                .pop_front()
                .expect("usb serial rx queue length changed while locked");
        }
        count
    }

    fn push_rx(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut dropped = 0usize;
        {
            let mut queue = self.rx_queue.lock();
            for &byte in bytes {
                if queue.len() < USB_SERIAL_RX_QUEUE_CAP {
                    queue.push_back(byte);
                } else {
                    dropped += 1;
                }
            }
        }
        if dropped > 0 {
            let previous = self.dropped_rx.fetch_add(dropped, Ordering::AcqRel);
            if previous == 0 {
                warn!("usb-serial: ttyUSB{} RX queue full", self.index);
            }
        }
        unsafe { self.input_source.wake(IoEvents::IN) };
    }

    fn request_session_teardown(&self, failed_session: Option<&Arc<UsbSerialSession>>) {
        let should_close = {
            let session = self.session.lock();
            match (session.as_ref(), failed_session) {
                (Some(current), Some(failed)) => Arc::ptr_eq(current, failed),
                (Some(_), None) => true,
                (None, _) => false,
            }
        };

        if should_close {
            self.session_closing.store(true, Ordering::Release);
            self.started.store(false, Ordering::Release);
            self.clear_tx_queue();
            unsafe { self.input_source.wake(IoEvents::IN) };
            // Do not cancel an in-flight RX transfer from here. The kmod xHCI
            // backend does not yet provide a Stop Endpoint based cancellation
            // path, so teardown is deferred until the RX worker observes a real
            // USB completion or device error and can release the session.
            if !self.rx_worker_started.load(Ordering::Acquire) {
                self.finish_session_teardown(None);
            }
        }
    }

    fn finish_session_teardown(&self, failed_session: Option<&Arc<UsbSerialSession>>) {
        self.session_closing.store(true, Ordering::Release);
        let dropped = {
            let mut session = self.session.lock();
            let should_clear = match (session.as_ref(), failed_session) {
                (Some(current), Some(failed)) => Arc::ptr_eq(current, failed),
                (Some(_), None) => true,
                (None, _) => false,
            };
            should_clear.then(|| session.take()).flatten()
        };

        if dropped.is_some() {
            self.started.store(false, Ordering::Release);
            self.rx_queue.lock().clear();
            self.clear_tx_queue();
            unsafe { self.input_source.wake(IoEvents::IN) };
        }
        self.rx_worker_started.store(false, Ordering::Release);
        self.session_closing.store(false, Ordering::Release);
    }

    fn bulk_in_rx(&self, session: &UsbSerialSession, buf: &mut [u8]) -> AxResult<usize> {
        session.handle.bulk_in(session.port.bulk_in, buf)
    }

    fn start_rx_worker(self: &Arc<Self>) {
        if self
            .rx_worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let backend = self.clone();
        ax_task::spawn_with_name(
            move || {
                let mut buf = [0u8; USB_SERIAL_RX_CHUNK];
                loop {
                    if backend.session_closing.load(Ordering::Acquire) {
                        // RX worker is the only path that can safely complete
                        // teardown while a synchronous bulk IN may be in
                        // flight.
                        backend.finish_session_teardown(None);
                        break;
                    }

                    let session = match backend.ensure_session() {
                        Ok(session) => session,
                        Err(err) => {
                            if backend.session_closing.load(Ordering::Acquire) {
                                backend.finish_session_teardown(None);
                            } else {
                                warn!(
                                    "usb-serial: ttyUSB{} RX worker failed to attach: {err:?}",
                                    backend.index
                                );
                                backend.rx_worker_started.store(false, Ordering::Release);
                            }
                            break;
                        }
                    };
                    match backend.bulk_in_rx(&session, &mut buf) {
                        Ok(0) => ax_task::yield_now(),
                        Ok(actual) => {
                            backend.push_rx(&buf[..actual.min(buf.len())]);
                            if backend.session_closing.load(Ordering::Acquire) {
                                backend.finish_session_teardown(Some(&session));
                                break;
                            }
                        }
                        Err(err) => {
                            backend.finish_session_teardown(Some(&session));
                            warn!(
                                "usb-serial: ttyUSB{} RX worker stopped: {err:?}",
                                backend.index
                            );
                            break;
                        }
                    }
                }
            },
            "usb-serial-rx".to_string(),
        );
    }

    fn start_tx_worker(self: &Arc<Self>) {
        if self
            .tx_worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let backend = self.clone();
        ax_task::spawn_with_name(
            move || loop {
                let result = {
                    let _guard = backend.output_lock.lock();
                    backend.drain_tx_queue_locked()
                };
                if let Err(err) = result {
                    warn!(
                        "usb-serial: ttyUSB{} TX worker stopped: {err:?}",
                        backend.index
                    );
                    backend.tx_worker_started.store(false, Ordering::Release);
                    break;
                }

                backend.tx_worker_started.store(false, Ordering::Release);
                // Avoid a lost wakeup: if a producer queued more bytes after
                // the queue was drained, take the worker flag again and loop.
                if backend.tx_queue.lock().is_empty()
                    || backend
                        .tx_worker_started
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                {
                    break;
                }
            },
            "usb-serial-tx".to_string(),
        );
    }
}

impl TtyRead for UsbSerialReader {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        if !self.backend.started.load(Ordering::Acquire) {
            return 0;
        }
        self.backend.drain_rx(buf)
    }
}

impl TtyWrite for UsbSerialWriter {
    fn open(&self) -> AxResult<()> {
        self.backend.ensure_started()
    }

    fn write(&self, buf: &[u8]) {
        if let Err(err) = self.backend.write_bytes(buf) {
            warn!(
                "usb-serial: ttyUSB{} write failed: {err:?}",
                self.backend.index
            );
        }
    }

    fn try_write(&self, buf: &[u8]) -> usize {
        self.backend.try_queue_bytes(buf)
    }

    fn termios_changed(&self, old: &Termios2, new: &Termios2) {
        let Some(new_baud) = new.baudrate() else {
            return;
        };
        if old.baudrate() == Some(new_baud) {
            return;
        }
        if let Err(err) = self.backend.set_baudrate(new_baud) {
            warn!(
                "usb-serial: ttyUSB{} failed to set baudrate {new_baud}: {err:?}",
                self.backend.index
            );
        }
    }
}

impl Drop for UsbSerialSession {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.port.interface);
    }
}

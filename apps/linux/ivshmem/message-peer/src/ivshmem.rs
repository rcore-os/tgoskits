use std::{
    ffi::{CStr, c_char, c_int, c_void},
    fmt,
    fs::{File, OpenOptions},
    io,
    marker::PhantomData,
    mem,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::Path,
    ptr::{self, NonNull},
    sync::atomic::{self, Ordering},
};

use anyhow::{Context, Result, anyhow, bail};
use axivc::{IvcEndpoints, IvshmemMessagePort, IvshmemSessionState};

use crate::record::{PendingEndpoint, session_lost};

const BAR_REGISTERS: u8 = 0;
const BAR_SHARED: u8 = 2;
const REGISTER_ID: usize = 0x00;
const REGISTER_MAX_PEERS: usize = 0x04;
const REGISTER_DOORBELL: usize = 0x0c;
const REGISTER_PAGE_SIZE: usize = 0x1000;
const REGISTER_COMMON_SIZE: usize = 0x18;
const REGISTER_OUTPUT_SIZE: usize = 0x1c;

const EXPECTED_MAX_PEERS: u32 = 2;
const DOORBELL_VECTOR: u16 = 0;

const PAGE_SIZE: usize = 0x1000;
const STATE_TABLE_SIZE: usize = PAGE_SIZE;
const EXPECTED_COMMON_SIZE: usize = 0;

// This advisory lock coordinates every cooperating Linux writer. The guest
// initramfs fixes /tmp, and those writers must share this exact path and mount
// namespace; deriving it from TMPDIR would let environment differences bypass
// the lock.
const WRITER_LOCK_PATH: &str = "/tmp/linux-ivshmem-subscriber.lock";

#[repr(C)]
struct FfiDevice {
    _opaque: [u8; 0],
}

#[repr(C)]
struct FfiBackend {
    _opaque: [u8; 0],
}

#[repr(C)]
enum FfiBackendKind {
    Interrupt = 1,
}

unsafe extern "C" {
    fn ivshmem_find_device(bdf: *const c_char, out: *mut *mut FfiDevice) -> c_int;
    fn ivshmem_device_close(device: *mut FfiDevice);
    fn ivshmem_enable_device(device: *mut FfiDevice) -> c_int;
    fn ivshmem_map_bar(
        device: *mut FfiDevice,
        bar: u8,
        map: *mut *mut c_void,
        size: *mut usize,
    ) -> c_int;
    fn ivshmem_observe_peer_epoch(device: *const FfiDevice, peer: u16, epoch: *mut u64) -> c_int;
    fn ivshmem_read_reg32(device: *const FfiDevice, offset: u32) -> u32;
    fn ivshmem_write_reg32(device: *const FfiDevice, offset: u32, value: u32);
    fn ivshmem_backend_open(
        device: *mut FfiDevice,
        kind: FfiBackendKind,
        out: *mut *mut FfiBackend,
    ) -> c_int;
    fn ivshmem_backend_wait_event(backend: *mut FfiBackend, timeout_ms: c_int) -> c_int;
    fn ivshmem_backend_close(backend: *mut FfiBackend);
    fn ivshmem_strerror(error: c_int) -> *const c_char;
}

/// One verified ivshmem device with exclusive process-level writer ownership.
///
/// The profile is a two-peer IVC link with this guest as peer 0 or 1. Section
/// boundaries use the geometry advertised by BAR0, while BAR2 may be larger
/// than the exact profile requirement for fixed hardware guests.
pub struct ExclusiveDevice {
    device: Device,
    registers: Mapping,
    shared: Mapping,
    peer_id: u32,
    max_peers: u32,
    common_size: usize,
    output_size: usize,
    session_taken: bool,
    _writer_lock: WriterLock,
}

impl ExclusiveDevice {
    pub fn open() -> Result<Self> {
        let writer_lock = WriterLock::acquire()?;
        let device = Device::find()?;
        device.enable()?;

        let registers = device.map_bar(BAR_REGISTERS)?;
        if registers.len < REGISTER_PAGE_SIZE {
            bail!(
                "BAR0 is {} bytes, expected at least {REGISTER_PAGE_SIZE:#x}",
                registers.len
            );
        }

        let peer_id = device.read_reg32(REGISTER_ID);
        let max_peers = device.read_reg32(REGISTER_MAX_PEERS);
        let common_size = usize::try_from(device.read_reg32(REGISTER_COMMON_SIZE))
            .context("ivshmem common size does not fit usize")?;
        let output_size = usize::try_from(device.read_reg32(REGISTER_OUTPUT_SIZE))
            .context("ivshmem output size does not fit usize")?;
        if peer_id > 1 || max_peers != EXPECTED_MAX_PEERS {
            bail!(
                "ivshmem profile mismatch: peer={peer_id} max_peers={max_peers}, expected peer=0 \
                 or 1 and max_peers={EXPECTED_MAX_PEERS}"
            );
        }
        if output_size == 0 {
            bail!("ivshmem section geometry is unavailable (output_size=0)");
        }
        if !common_size.is_multiple_of(PAGE_SIZE) || !output_size.is_multiple_of(PAGE_SIZE) {
            bail!(
                "ivshmem section geometry is not page aligned: common={common_size:#x} \
                 output={output_size:#x}"
            );
        }
        if common_size != EXPECTED_COMMON_SIZE {
            bail!("ivshmem common size is {common_size:#x}, expected {EXPECTED_COMMON_SIZE:#x}");
        }
        let required_bar2_size = STATE_TABLE_SIZE
            .checked_add(common_size)
            .and_then(|size| {
                usize::try_from(max_peers)
                    .ok()
                    .and_then(|peers| peers.checked_mul(output_size))
                    .and_then(|outputs| size.checked_add(outputs))
            })
            .ok_or_else(|| anyhow!("ivshmem section geometry overflows BAR2"))?;

        let shared = device.map_bar(BAR_SHARED)?;
        if !shared.len.is_power_of_two() || !shared.len.is_multiple_of(PAGE_SIZE) {
            bail!(
                "BAR2 size {:#x} is not a page-aligned power of two",
                shared.len
            );
        }
        if required_bar2_size > shared.len {
            bail!(
                "BAR2 is too small for the advertised geometry: {:#x} < {required_bar2_size:#x}",
                shared.len
            );
        }

        Ok(Self {
            device,
            registers,
            shared,
            peer_id,
            max_peers,
            common_size,
            output_size,
            session_taken: false,
            _writer_lock: writer_lock,
        })
    }

    pub const fn peer_id(&self) -> u32 {
        self.peer_id
    }

    pub const fn max_peers(&self) -> u32 {
        self.max_peers
    }

    pub const fn common_size(&self) -> usize {
        self.common_size
    }

    pub const fn output_size(&self) -> usize {
        self.output_size
    }

    pub const fn bar2_size(&self) -> usize {
        self.shared.len
    }

    pub const fn remote_peer_id(&self) -> u16 {
        if self.peer_id == 0 { 1 } else { 0 }
    }

    /// Creates the process's only event backend and Message V1 port.
    pub fn take_session<'a>(&'a mut self) -> Result<MessageSession<'a>> {
        if self.session_taken {
            bail!("this process already created its ivshmem message session");
        }
        self.session_taken = true;

        let registers = Registers {
            device: self.device.raw,
            len: self.registers.len,
            max_peers: self.max_peers,
            local_peer_id: u16::try_from(self.peer_id)
                .context("ivshmem local peer does not fit u16")?,
            remote_peer_id: self.remote_peer_id(),
            _device: PhantomData,
        };
        let mut local_epoch = PeerEpoch::default();
        local_epoch.observe(&registers, registers.local_peer_id)?;
        let mut remote_epoch = PeerEpoch::default();
        remote_epoch.observe(&registers, registers.remote_peer_id)?;
        let shared = SharedBar {
            base: self.shared.pointer,
            len: self.shared.len,
            peer_id: self.peer_id,
            max_peers: self.max_peers,
            common_size: self.common_size,
            output_size: self.output_size,
            _device: PhantomData,
        };
        let backend = EventBackend::open(self)?;
        let port = shared.attach_message_port(u32::from(self.remote_peer_id()))?;
        Ok(MessageSession {
            port,
            backend,
            registers,
            local_epoch,
            remote_epoch,
            started: false,
        })
    }

    /// Creates the process's single logical endpoint and publishes this
    /// Linux peer's incarnation before waiting for the other peer.
    pub fn take_endpoint(&mut self) -> Result<PendingEndpoint<'_>> {
        let mut session = self.take_session()?;
        let local_incarnation = session.begin_session()?;
        session.ring_peer()?;
        Ok(PendingEndpoint::new(session, local_incarnation))
    }
}

/// The one interrupt backend and Message V1 port owned by this process.
///
/// Fields drop in declaration order, so the port and backend are released
/// before the `ExclusiveDevice` that owns their BAR mappings.
pub struct MessageSession<'a> {
    port: IvshmemMessagePort<'a>,
    backend: EventBackend<'a>,
    registers: Registers<'a>,
    local_epoch: PeerEpoch,
    remote_epoch: PeerEpoch,
    started: bool,
}

impl<'a> MessageSession<'a> {
    pub fn begin_session(&mut self) -> Result<std::num::NonZeroU32> {
        if self.started {
            bail!("the ivshmem Message V1 session was already initialized");
        }
        self.started = true;
        // SAFETY: `take_session` runs once per process and the writer lock is
        // held by the backing ExclusiveDevice. No endpoint exists yet.
        unsafe { self.port.begin_session() }
            .context("failed to publish the ivshmem Message V1 session")
    }

    pub fn poll_handshake(&mut self) -> Result<IvshmemSessionState> {
        self.port
            .poll_handshake()
            .context("ivshmem Message V1 handshake failed")
    }

    pub fn session_state(&self) -> Result<IvshmemSessionState> {
        self.port
            .session_state()
            .context("failed to inspect the ivshmem Message V1 state")
    }

    pub(crate) fn observe_epochs(&mut self) -> Result<()> {
        self.local_epoch
            .observe(&self.registers, self.registers.local_peer_id)?;
        self.remote_epoch
            .observe(&self.registers, self.registers.remote_peer_id)
    }

    pub(crate) fn epochs_current(&self) -> Result<bool> {
        Ok(self
            .local_epoch
            .is_current(&self.registers, self.registers.local_peer_id)?
            && self
                .remote_epoch
                .is_current(&self.registers, self.registers.remote_peer_id)?)
    }

    pub fn try_connect(&mut self) -> Result<IvcEndpoints<'a>> {
        self.port
            .try_connect()
            .context("failed to connect the ivshmem Message V1 session")
    }

    pub fn ring_peer(&self) -> Result<()> {
        self.registers.ring_peer()
    }

    pub fn wait_event(&self, timeout_ms: c_int) -> Result<WaitResult> {
        self.backend.wait_event(timeout_ms)
    }

    pub fn invalidate_session(&mut self) {
        if !self.started {
            return;
        }
        // SAFETY: every endpoint derived from this port is dropped before
        // MessageSession or LinuxMessageEndpoint invalidation runs, and the
        // writer lock plus single-session flag keep this output exclusively
        // owned by the process.
        unsafe { self.port.invalidate_session() };
        self.started = false;
        // Best effort: wake the peer after publishing the invalid header.
        let _ = self.registers.ring_peer();
    }
}

impl Drop for MessageSession<'_> {
    fn drop(&mut self) {
        self.invalidate_session();
    }
}

#[derive(Default)]
struct PeerEpoch {
    baseline: Option<u64>,
}

impl PeerEpoch {
    fn observe(&mut self, registers: &Registers<'_>, peer_id: u16) -> Result<()> {
        let observed = registers.observe_epoch(peer_id)?;
        // Zero means this peer's host epoch has not been published yet.
        if observed == 0 {
            return Ok(());
        }
        match self.baseline {
            Some(expected) if observed != expected => Err(session_lost(format!(
                "ivshmem peer epoch changed from {expected:#x} to {observed:#x}"
            ))),
            Some(_) => Ok(()),
            None => {
                self.baseline = Some(observed);
                Ok(())
            }
        }
    }

    fn is_current(&self, registers: &Registers<'_>, peer_id: u16) -> Result<bool> {
        let observed = registers.observe_epoch(peer_id)?;
        Ok(match self.baseline {
            Some(expected) => observed == 0 || observed == expected,
            None => true,
        })
    }
}

struct Registers<'a> {
    device: NonNull<FfiDevice>,
    len: usize,
    max_peers: u32,
    local_peer_id: u16,
    remote_peer_id: u16,
    _device: PhantomData<&'a ExclusiveDevice>,
}

impl Registers<'_> {
    fn observe_epoch(&self, peer_id: u16) -> Result<u64> {
        let mut epoch = 0;
        // SAFETY: the device handle is live for this borrow and `epoch` is a
        // valid initialized output slot for exactly one u64 value.
        let result =
            unsafe { ivshmem_observe_peer_epoch(self.device.as_ptr(), peer_id, &mut epoch) };
        check_adapter("observe ivshmem peer epoch", result)?;
        Ok(epoch)
    }

    /// Publishes all prior shared-memory stores before ringing the peer.
    pub fn ring_peer(&self) -> Result<()> {
        let target = u32::from(self.remote_peer_id);
        if target >= self.max_peers || target > u32::from(u16::MAX) {
            bail!(
                "cannot ring ivshmem peer {target} with max_peers={}",
                self.max_peers
            );
        }
        let value = (target << 16) | u32::from(DOORBELL_VECTOR);
        self.check_register(REGISTER_DOORBELL, mem::size_of::<u32>())?;

        atomic::fence(Ordering::SeqCst);
        // SAFETY: the device and BAR0 mapping are owned by ExclusiveDevice,
        // which remains borrowed for `self`, and the C adapter validated the
        // fixed 4 KiB register page before returning the mapping.
        unsafe {
            ivshmem_write_reg32(self.device.as_ptr(), REGISTER_DOORBELL as u32, value);
        }
        Ok(())
    }

    fn check_register(&self, offset: usize, width: usize) -> Result<()> {
        let end = offset
            .checked_add(width)
            .ok_or_else(|| anyhow!("BAR0 register range overflows"))?;
        if !offset.is_multiple_of(mem::align_of::<u32>()) || end > self.len {
            bail!("BAR0 register access {offset:#x}..{end:#x} is invalid");
        }
        Ok(())
    }
}

struct SharedBar<'a> {
    base: NonNull<u8>,
    len: usize,
    peer_id: u32,
    max_peers: u32,
    common_size: usize,
    output_size: usize,
    _device: PhantomData<&'a ExclusiveDevice>,
}

impl<'a> SharedBar<'a> {
    /// Attaches the local and ArceOS owner-only output sections.
    pub fn attach_message_port(&self, peer: u32) -> Result<IvshmemMessagePort<'a>> {
        if peer >= self.max_peers {
            bail!("ivshmem peer {peer} is outside 0..{}", self.max_peers);
        }
        if peer == self.peer_id {
            bail!(
                "ivshmem message peer must differ from local peer {}",
                self.peer_id
            );
        }

        let local = self.output_section(self.peer_id)?;
        let remote = self.output_section(peer)?;
        let local_start = local.pointer.as_ptr().addr();
        let remote_start = remote.pointer.as_ptr().addr();
        let local_end = local_start
            .checked_add(local.len)
            .ok_or_else(|| anyhow!("local output range overflows"))?;
        let remote_end = remote_start
            .checked_add(remote.len)
            .ok_or_else(|| anyhow!("remote output range overflows"))?;
        if local_start < remote_end && remote_start < local_end {
            bail!("local and peer ivshmem output sections overlap");
        }

        // SAFETY: `SharedBar` is created only by a verified ExclusiveDevice.
        // Its writer lock covers every Linux writer in the guest profile, so
        // the local output has one protocol writer. Both page-aligned
        // ranges are checked in the live BAR2 mapping; the local range is
        // writable and the peer range is consumed through attach's read-only
        // atomic view. No endpoint from an earlier process can remain after
        // its own writer lock and address space are gone.
        unsafe {
            IvshmemMessagePort::attach(
                local.pointer.as_ptr(),
                local.len,
                remote.pointer.as_ptr().cast_const(),
                remote.len,
            )
        }
        .context("failed to attach the ivshmem Message V1 port")
    }

    fn output_section(&self, peer: u32) -> Result<OutputSection<'a>> {
        if peer >= self.max_peers {
            bail!("ivshmem peer {peer} is outside 0..{}", self.max_peers);
        }
        if !self.output_size.is_multiple_of(PAGE_SIZE) {
            bail!("ivshmem output section size is not page aligned");
        }
        let peer_index = usize::try_from(peer).context("ivshmem peer does not fit usize")?;
        let relative = peer_index
            .checked_mul(self.output_size)
            .ok_or_else(|| anyhow!("ivshmem output offset overflow"))?;
        let offset = STATE_TABLE_SIZE
            .checked_add(self.common_size)
            .ok_or_else(|| anyhow!("ivshmem output offset overflow"))?
            .checked_add(relative)
            .ok_or_else(|| anyhow!("ivshmem output offset overflow"))?;
        let end = offset
            .checked_add(self.output_size)
            .ok_or_else(|| anyhow!("ivshmem output range overflow"))?;
        if !offset.is_multiple_of(PAGE_SIZE) || end > self.len {
            bail!(
                "ivshmem peer {peer} output {offset:#x}..{end:#x} is outside BAR2 length {:#x}",
                self.len
            );
        }
        let pointer = self
            .base
            .as_ptr()
            .addr()
            .checked_add(offset)
            .ok_or_else(|| anyhow!("ivshmem output address overflow"))?;
        if !pointer.is_multiple_of(PAGE_SIZE) {
            bail!("ivshmem peer {peer} output address is not page aligned");
        }

        // SAFETY: the checked relative range belongs to the live BAR2 mapping,
        // whose base is non-null; adding an in-range offset cannot wrap.
        let pointer = unsafe { NonNull::new_unchecked(self.base.as_ptr().add(offset)) };
        Ok(OutputSection {
            pointer,
            len: self.output_size,
            _bar: PhantomData,
        })
    }
}

struct OutputSection<'a> {
    pointer: NonNull<u8>,
    len: usize,
    _bar: PhantomData<&'a SharedBar<'a>>,
}

struct EventBackend<'a> {
    backend: NonNull<FfiBackend>,
    _device: PhantomData<&'a ExclusiveDevice>,
}

impl<'a> EventBackend<'a> {
    pub fn wait_event(&self, timeout_ms: c_int) -> Result<WaitResult> {
        // SAFETY: the backend is non-null, borrowed from the live device, and
        // was opened with the interrupt-kind enum below.
        let result = unsafe { ivshmem_backend_wait_event(self.backend.as_ptr(), timeout_ms) };
        match result {
            0 => Ok(WaitResult::Timeout),
            1 => Ok(WaitResult::Interrupt),
            code => Err(AdapterError::new("wait for ivshmem MSI-X/UIO event", code).into()),
        }
    }

    fn open(device: &'a ExclusiveDevice) -> Result<Self> {
        let mut backend = ptr::null_mut();
        // SAFETY: the device handle is live and `backend` is a valid output
        // slot. The only construction path requests INTERRUPT, so polling is
        // never selected as a fallback.
        let result = unsafe {
            ivshmem_backend_open(
                device.device.raw.as_ptr(),
                FfiBackendKind::Interrupt,
                &mut backend,
            )
        };
        check_adapter("open ivshmem interrupt backend", result)?;
        let backend = NonNull::new(backend)
            .ok_or_else(|| anyhow!("ivshmem_backend_open returned a null backend"))?;
        Ok(Self {
            backend,
            _device: PhantomData,
        })
    }
}

impl Drop for EventBackend<'_> {
    fn drop(&mut self) {
        // SAFETY: EventBackend owns the non-null C backend exactly once. Its
        // device borrow prevents the mapping owner from dropping first.
        unsafe {
            ivshmem_backend_close(self.backend.as_ptr());
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitResult {
    Interrupt,
    Timeout,
}

struct Device {
    raw: NonNull<FfiDevice>,
}

impl Device {
    fn find() -> Result<Self> {
        let mut device = ptr::null_mut();
        // SAFETY: NULL requests the adapter's verified device auto-discovery,
        // and `device` is a valid initialized output slot.
        let result = unsafe { ivshmem_find_device(ptr::null(), &mut device) };
        check_adapter("discover ivshmem device", result)?;
        let raw = NonNull::new(device)
            .ok_or_else(|| anyhow!("ivshmem_find_device returned a null device"))?;
        Ok(Self { raw })
    }

    fn enable(&self) -> Result<()> {
        // SAFETY: `raw` is a live, uniquely owned device handle.
        let result = unsafe { ivshmem_enable_device(self.raw.as_ptr()) };
        check_adapter("enable ivshmem PCI memory decoding", result)
    }

    fn map_bar(&self, bar: u8) -> Result<Mapping> {
        let mut pointer = ptr::null_mut();
        let mut len = 0;
        // SAFETY: `raw` is live and both output pointers are valid.
        let result = unsafe { ivshmem_map_bar(self.raw.as_ptr(), bar, &mut pointer, &mut len) };
        check_adapter("map ivshmem BAR", result)?;
        let pointer = NonNull::new(pointer.cast::<u8>())
            .ok_or_else(|| anyhow!("ivshmem BAR{bar} mapping returned a null pointer"))?;
        if len == 0 {
            bail!("ivshmem BAR{bar} mapping returned a zero length");
        }
        Ok(Mapping { pointer, len })
    }

    fn read_reg32(&self, offset: usize) -> u32 {
        // SAFETY: the device owns a mapped BAR0 register page for its whole
        // lifetime, and the fixed offsets used here are aligned and in range.
        unsafe { ivshmem_read_reg32(self.raw.as_ptr(), offset as u32) }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: Device owns this non-null C handle exactly once. EventBackend's
        // borrow prevents this drop while a backend still references the device.
        unsafe {
            ivshmem_device_close(self.raw.as_ptr());
        }
    }
}

struct Mapping {
    pointer: NonNull<u8>,
    len: usize,
}

struct WriterLock {
    _file: File,
}

impl WriterLock {
    fn acquire() -> Result<Self> {
        let path = Path::new(WRITER_LOCK_PATH);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .with_context(|| format!("failed to open ivshmem writer lock {}", path.display()))?;
        if !file
            .metadata()
            .with_context(|| format!("failed to inspect ivshmem writer lock {}", path.display()))?
            .file_type()
            .is_file()
        {
            bail!(
                "ivshmem writer lock {} is not a regular file",
                path.display()
            );
        }

        // SAFETY: `file` owns a valid descriptor and flock only changes the
        // open-file-description lock state.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.raw_os_error(),
                Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN
            ) {
                return Err(WriterLockBusy.into());
            }
            return Err(error)
                .with_context(|| format!("failed to lock ivshmem writer file {}", path.display()));
        }
        Ok(Self { _file: file })
    }
}

#[derive(Debug)]
pub(crate) struct AdapterError {
    operation: &'static str,
    code: c_int,
}

impl AdapterError {
    const fn new(operation: &'static str, code: c_int) -> Self {
        Self { operation, code }
    }

    pub(crate) const fn code(&self) -> c_int {
        self.code
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // SAFETY: the adapter returns a process-static C string for every
        // integer value, including unknown errors.
        let pointer = unsafe { ivshmem_strerror(self.code) };
        if pointer.is_null() {
            return write!(
                formatter,
                "{} failed with adapter error {}",
                self.operation, self.code
            );
        }
        // SAFETY: the non-null pointer is the adapter's static NUL-terminated
        // error string and is not retained beyond this formatting operation.
        let message = unsafe { CStr::from_ptr(pointer) }.to_string_lossy();
        write!(
            formatter,
            "{} failed ({}): {message}",
            self.operation, self.code
        )
    }
}

impl std::error::Error for AdapterError {}

#[derive(Debug)]
pub(crate) struct WriterLockBusy;

impl fmt::Display for WriterLockBusy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("another Linux process owns the ivshmem writer lock")
    }
}

impl std::error::Error for WriterLockBusy {}

fn check_adapter(operation: &'static str, code: c_int) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(AdapterError::new(operation, code).into())
    }
}

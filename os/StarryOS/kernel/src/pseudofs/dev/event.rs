use alloc::{collections::VecDeque, format, string::ToString, sync::Arc, vec, vec::Vec};
use core::{
    any::Any,
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
    task::Context,
    time::Duration,
};

/// Number of registered `/dev/input/event*` nodes. Populated by
/// [`input_devices`] at boot and read by sysfs so
/// `/sys/class/input/event<N>` matches reality.
static EVENT_DEVICE_COUNT: AtomicU32 = AtomicU32::new(0);

/// Returns the number of `/dev/input/event*` devices currently exposed.
pub fn input_device_count() -> u32 {
    EVENT_DEVICE_COUNT.load(Ordering::Acquire)
}

use ax_errno::{AxError, AxResult};
use ax_input::{ErasedInputDevice, Event, EventType, InputDevice, InputDeviceId, InputError};
use ax_runtime::hal::time::{monotonic_time_nanos, wall_time};
use ax_sync::spin::SpinNoIrq as Mutex;
use axfs_ng_vfs::{DeviceId, NodeFlags, NodeType, VfsResult};
use axpoll::{IoEvents, PollSet, Pollable};
use bitmaps::Bitmap;
use linux_raw_sys::{
    general::{__kernel_old_time_t, __kernel_suseconds_t},
    ioctl::{EVIOCGID, EVIOCGRAB, EVIOCGVERSION},
};
use zerocopy::{FromBytes, Immutable, IntoBytes};

use crate::{
    mm::UserPtr,
    pseudofs::{Device, DeviceOps, DirMapping, SimpleFs},
};
const KEY_CNT: usize = EventType::Key.bits_count();

/// Bound on the in-kernel evdev buffer. Linux uses a per-client ring of
/// 64 entries by default; we hold a bit more headroom so a 20-key burst
/// (key down + key up + EV_SYN per key = 60 entries) never drops events
/// before userspace drains it. When the queue is full we follow Linux's
/// behavior and drop the oldest entry rather than blocking the driver.
const READ_AHEAD_CAP: usize = 256;

/// If no IRQ event has been observed within this window, IRQ delivery is
/// considered broken and the polling fallback runs actively.
const IRQ_ALIVE_NS: u64 = 1_000_000_000; // 1 second

struct Inner {
    device: ErasedInputDevice,
    read_ahead: VecDeque<(Duration, Event)>,
    key_state: Bitmap<KEY_CNT>,
}
impl Inner {
    /// Drain everything the driver currently has buffered into `read_ahead`,
    /// updating cached key state along the way. Stops at the first
    /// `InputError::Again` (driver queue empty) or after a hard ceiling of
    /// `READ_AHEAD_CAP` pulls per call to bound a single pass.
    ///
    /// Returns `true` if at least one event is now queued for userspace.
    fn drain_into_queue(&mut self) -> bool {
        for _ in 0..READ_AHEAD_CAP {
            match self.device.read_event() {
                Ok(event) => {
                    if event.event_type == EventType::Key as u16 {
                        if event.value == 0 {
                            self.key_state.set(event.code as usize, false);
                        } else if event.value == 1 {
                            self.key_state.set(event.code as usize, true);
                        }
                    }
                    if self.read_ahead.len() >= READ_AHEAD_CAP {
                        // Mirror Linux evdev: drop oldest on overflow so
                        // the most recent input wins. Keeps the driver
                        // ring from stalling under a burst we cannot
                        // forward to a slow reader.
                        self.read_ahead.pop_front();
                    }
                    self.read_ahead.push_back((wall_time(), event));
                }
                Err(InputError::Again) => break,
                Err(err) => {
                    warn!("Failed to read event: {err:?}");
                    break;
                }
            }
        }
        !self.read_ahead.is_empty()
    }

    fn has_event(&mut self) -> bool {
        self.drain_into_queue()
    }
}

/// Linux `INPUT_PROP_CNT` — the property bitmap is 4 bytes (32 properties).
const INPUT_PROP_CNT: usize = 0x20;
/// Linux `INPUT_PROP_POINTER` — emulates a relative pointer or maps absolute
/// coordinates to screen space. libinput hides the cursor on absolute-axis
/// devices that do not advertise this until proximity is reported.
const INPUT_PROP_POINTER: usize = 0x00;
/// Linux `INPUT_PROP_DIRECT` — direct-mapped axes (touchscreens).
const INPUT_PROP_DIRECT: usize = 0x01;

/// Linux uapi `struct input_absinfo` — six `i32`s returned by
/// `EVIOCGABS(axis)`.
#[repr(C)]
#[derive(Default, Clone, Copy, FromBytes, IntoBytes, Immutable)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

/// Maximum number of absolute axes Linux's EVIOCGABS encodes (0..0x3F).
const ABS_MAX: usize = 0x40;

pub struct EventDev {
    inner: Mutex<Inner>,
    waiters: PollSet,
    /// IRQ line the underlying driver advertises, when available.
    irq: Option<usize>,
    irq_handle: spin::Once<ax_runtime::hal::irq::IrqHandle>,
    /// Monotonic timestamp (ns) of the last successful IRQ drain.
    /// When this is recent, IRQ delivery is considered healthy and the
    /// polling fallback stays at low frequency even with active waiters.
    last_irq_event: AtomicU64,
    ev_bits: Bitmap<{ EventType::COUNT as usize }>,
    /// Cached `EVIOCGPROP` bitmap. Computed once at probe from the driver's
    /// raw bits with a synthesized `INPUT_PROP_POINTER` for absolute or
    /// relative pointing devices that aren't touchscreens. QEMU's
    /// virtio-mouse / virtio-tablet do not set the bit themselves, so
    /// libinput would otherwise classify the tablet as a graphics tablet
    /// and suppress the cursor pending a never-firing `BTN_TOOL_PEN`.
    prop_bits: [u8; INPUT_PROP_CNT.div_ceil(8)],
    /// Cached `EV_ABS` bitmap. Used by `EVIOCGABS` to refuse axes the
    /// device doesn't advertise with `EINVAL`, matching Linux's
    /// `evdev_handle_get_val` behavior. virtio-drivers reports the
    /// underlying `Error::IoError` when the AbsInfo selector has size 0,
    /// which would otherwise surface as EIO and confuse libinput.
    abs_bits: [u8; ABS_MAX.div_ceil(8)],
}

impl EventDev {
    pub fn new(mut device: ErasedInputDevice) -> Self {
        let mut ev_bits = Bitmap::new();
        for i in 0..EventType::COUNT {
            let Some(ty) = EventType::from_repr(i) else {
                continue;
            };
            if device
                .get_event_bits(ty, &mut [])
                .is_ok_and(|success| success)
            {
                ev_bits.set(i as usize, true);
            }
        }

        let mut prop_bits = [0u8; INPUT_PROP_CNT.div_ceil(8)];
        let prop_bits_reliable = match device.get_prop_bits(&mut prop_bits) {
            Ok(_) => true,
            Err(err) => {
                warn!("Failed to get input property bits: {err:?}");
                false
            }
        };
        let is_touchscreen = prop_bits[INPUT_PROP_DIRECT / 8] & (1 << (INPUT_PROP_DIRECT % 8)) != 0;
        let has_axes =
            ev_bits.get(EventType::Relative as usize) || ev_bits.get(EventType::Absolute as usize);
        if prop_bits_reliable && has_axes && !is_touchscreen {
            prop_bits[INPUT_PROP_POINTER / 8] |= 1 << (INPUT_PROP_POINTER % 8);
        }

        let mut abs_bits = [0u8; ABS_MAX.div_ceil(8)];
        if ev_bits.get(EventType::Absolute as usize) {
            let _ = device.get_event_bits(EventType::Absolute, &mut abs_bits);
        }

        let irq = device.irq_num();
        Self {
            inner: Mutex::new(Inner {
                device,
                read_ahead: VecDeque::with_capacity(READ_AHEAD_CAP),
                key_state: Bitmap::new(),
            }),
            waiters: PollSet::new(),
            irq,
            irq_handle: spin::Once::new(),
            last_irq_event: AtomicU64::new(0),
            ev_bits,
            prop_bits,
            abs_bits,
        }
    }

    fn axis_supported(&self, axis: u8) -> bool {
        let bit = axis as usize;
        if bit >= ABS_MAX {
            return false;
        }
        self.abs_bits[bit / 8] & (1 << (bit % 8)) != 0
    }

    fn get_event_bits(&self, arg: usize, size: usize, ty: u8) -> AxResult<usize> {
        if ty == 0 {
            let bits = UserPtr::<u8>::from(arg).get_as_mut_slice(size)?;
            Ok(copy_bytes(self.ev_bits.as_bytes(), bits))
        } else {
            let ty = EventType::from_repr(ty).ok_or(AxError::InvalidInput)?;
            let mut kernel_bits = vec![0; size];
            {
                let mut inner = self.inner.lock();
                match inner.device.get_event_bits(ty, &mut kernel_bits) {
                    Ok(true) => {}
                    Ok(false) => {
                        debug!("No events for {ty:?}");
                    }
                    Err(err) => {
                        warn!("Failed to get event bits: {err:?}");
                    }
                }
            }
            let bytes = size.min(ty.bits_count().div_ceil(8));
            let bits = UserPtr::<u8>::from(arg).get_as_mut_slice(size)?;
            bits[..bytes].copy_from_slice(&kernel_bits[..bytes]);
            Ok(bytes)
        }
    }

    fn register_irq(self: &Arc<Self>) {
        // Always start a polling fallback.  IRQ handlers may fail to fire
        // (e.g. INTx routed but not delivered, MSI-X not negotiated, etc.)
        // and libinput blocks on epoll_wait forever.  The polling task
        // ensures events always reach userspace.
        self.start_polling();

        let Some(irq) = self.irq else {
            return;
        };

        let data = NonNull::from(self.as_ref()).cast();
        let request = ax_runtime::hal::irq::IrqRequest::new(event_dev_irq_handler, data)
            .share_mode(ax_runtime::hal::irq::ShareMode::Shared)
            .auto_enable(ax_runtime::hal::irq::AutoEnable::No);
        match ax_runtime::hal::irq::request_irq(irq, request) {
            Ok(handle) => {
                self.irq_handle.call_once(|| handle);
                self.inner.lock().device.enable_irq();
                if let Some(handle) = self.irq_handle.get().copied()
                    && let Err(err) = ax_runtime::hal::irq::enable_irq(handle)
                {
                    warn!("failed to enable evdev irq handler for irq {irq}: {err:?}");
                    self.inner.lock().device.disable_irq();
                }
            }
            Err(err) => {
                warn!("failed to register evdev irq handler for irq {irq}: {err:?}");
                self.inner.lock().device.disable_irq();
            }
        }
    }

    /// Spawn a background polling task that periodically drains input events
    /// from the device.  Used as a fallback when IRQ delivery is unreliable
    /// (e.g. MSI-X enabled but only INTx is resolved, or INTx routing fails).
    ///
    /// - IRQ alive → 200 ms idle (IRQ is the primary path).
    /// - IRQ stale, events found → 10 ms active (polling takes over).
    /// - IRQ stale, 20 consecutive empty drains → backoff to 200 ms.
    fn start_polling(self: &Arc<Self>) {
        let dev = self.clone();
        ax_task::spawn_with_name(
            move || {
                let mut empty_count = 0u32;
                loop {
                    let now = monotonic_time_nanos();
                    let irq = dev.last_irq_event.load(Ordering::Acquire);
                    let irq_alive = now.wrapping_sub(irq) <= IRQ_ALIVE_NS;

                    if irq_alive {
                        empty_count = 0;
                        ax_task::sleep(Duration::from_millis(200));
                        continue;
                    }

                    let mut inner = dev.inner.lock();
                    let ready = inner.drain_into_queue();
                    drop(inner);
                    if ready {
                        empty_count = 0;
                        unsafe { dev.waiters.wake(IoEvents::IN) };
                        ax_task::sleep(Duration::from_millis(10));
                    } else {
                        empty_count = empty_count.saturating_add(1);
                        let delay = if empty_count > 20 { 200 } else { 10 };
                        ax_task::sleep(Duration::from_millis(delay));
                    }
                }
            },
            "evdev-poll".into(),
        );
    }
}

unsafe fn event_dev_irq_handler(
    _ctx: ax_runtime::hal::irq::IrqContext,
    data: NonNull<()>,
) -> ax_runtime::hal::irq::IrqReturn {
    let event_dev = unsafe { data.cast::<EventDev>().as_ref() };
    // Use `lock()` rather than `try_lock()` so the virtio ISR is always
    // acknowledged. `SpinNoIrq` guarantees the holder has local IRQs
    // disabled, so this IRQ can only fire on a different CPU. Without the
    // ack, a level-triggered shared IRQ line stays asserted and can starve
    // other devices on the same line.
    let mut inner = event_dev.inner.lock();
    let event = inner.device.handle_irq();
    if event.input_ready && inner.drain_into_queue() {
        event_dev
            .last_irq_event
            .store(monotonic_time_nanos(), Ordering::Release);
        drop(inner);
        event_dev.waiters.wake_from_irq(IoEvents::IN);
        return ax_runtime::hal::irq::IrqReturn::Wake;
    }
    if event.handled {
        ax_runtime::hal::irq::IrqReturn::Handled
    } else {
        ax_runtime::hal::irq::IrqReturn::Unhandled
    }
}

fn copy_bytes(src: &[u8], dst: &mut [u8]) -> usize {
    let len = src.len().min(dst.len());
    dst[..len].copy_from_slice(&src[..len]);
    len
}

fn return_str(arg: usize, size: usize, s: &str) -> AxResult<usize> {
    let slice = UserPtr::<u8>::from(arg).get_as_mut_slice(size)?;
    Ok(copy_bytes(s.as_bytes(), slice))
}

fn input_error_to_ax_error(err: InputError) -> AxError {
    match err {
        InputError::AlreadyExists => AxError::AlreadyExists,
        InputError::Again => AxError::WouldBlock,
        InputError::BadState => AxError::BadState,
        InputError::InvalidInput | InputError::Unsupported => AxError::InvalidInput,
        InputError::Io => AxError::Io,
        InputError::NoMemory => AxError::NoMemory,
        InputError::ResourceBusy => AxError::ResourceBusy,
    }
}

fn return_zero_bits(arg: usize, size: usize, bits: usize) -> AxResult<usize> {
    let slice = UserPtr::<u8>::from(arg).get_as_mut_slice(size)?;
    let len = bits.div_ceil(8).min(slice.len());
    slice[..len].fill(0);
    Ok(len)
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, Immutable)]
pub struct KernelTimeval {
    pub tv_sec: __kernel_old_time_t,
    pub tv_usec: __kernel_suseconds_t,
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, Immutable)]
struct InputEvent {
    time: KernelTimeval,
    event_type: u16,
    code: u16,
    value: i32,
}

#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn ongkey() {
    core::hint::black_box(());
}

impl DeviceOps for EventDev {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if buf.len() < size_of::<InputEvent>() {
            return Err(AxError::InvalidInput);
        }
        let mut read = 0;
        let mut inner = self.inner.lock();
        // Drain the driver queue once up front so a single read() syscall
        // can return as many buffered events as the user buffer holds.
        inner.drain_into_queue();
        for out in buf.chunks_exact_mut(size_of::<InputEvent>()) {
            let Some((time, event)) = inner.read_ahead.pop_front() else {
                break;
            };
            let input_event = InputEvent {
                time: KernelTimeval {
                    tv_sec: time.as_secs() as _,
                    tv_usec: time.subsec_micros() as _,
                },
                event_type: event.event_type,
                code: event.code,
                value: event.value as _,
            };
            out.copy_from_slice(input_event.as_bytes());
            read += out.len();
        }
        if read == 0 {
            Err(AxError::WouldBlock)
        } else {
            Ok(read)
        }
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            EVIOCGVERSION => {
                *UserPtr::<u32>::from(arg).get_as_mut()? = 0x10001;
                Ok(0)
            }
            EVIOCGID => {
                let device_id = self.inner.lock().device.device_id();
                *UserPtr::<InputDeviceId>::from(arg).get_as_mut()? = device_id;
                Ok(0)
            }
            EVIOCGRAB => Ok(0),
            other => {
                // variable-length command
                let mut tmp = other;
                let nr = (tmp & 0xff) as u8;
                tmp >>= 8;
                let ty = (tmp & 0xff) as u8;
                tmp >>= 8;
                let size = (tmp & 0x3fff) as usize;
                tmp >>= 14;
                let dir = tmp & 0x3;

                if ty != b'E' {
                    warn!("unknown ioctl for evdev: {cmd} {arg}");
                    return Err(AxError::InvalidInput);
                }

                match dir {
                    // IOC_WRITE
                    1 => return Err(AxError::InvalidInput),
                    // IOC_READ
                    2 => {
                        #[allow(clippy::single_match)]
                        match nr {
                            // EVIOCGNAME
                            0x06 => {
                                let name = self.inner.lock().device.name().to_string();
                                return return_str(arg, size, &name);
                            }
                            // EVIOCGPHYS
                            0x07 => {
                                let location =
                                    self.inner.lock().device.physical_location().to_string();
                                return return_str(arg, size, &location);
                            }
                            // EVIOCGUNIQ
                            0x08 => {
                                let unique_id = self.inner.lock().device.unique_id().to_string();
                                return return_str(arg, size, &unique_id);
                            }
                            // EVIOCGPROP — device property bitmap. libinput
                            // uses INPUT_PROP_POINTER to keep the cursor
                            // visible on absolute-axis pointing devices like
                            // virtio-tablet; we synthesize the bit at probe
                            // for any non-touchscreen with REL/ABS axes.
                            0x09 => {
                                let slice = UserPtr::<u8>::from(arg).get_as_mut_slice(size)?;
                                return Ok(copy_bytes(&self.prop_bits, slice));
                            }
                            // EVIOCGKEY
                            0x18 => {
                                let key_state = {
                                    let inner = self.inner.lock();
                                    let bytes = inner.key_state.as_bytes();
                                    let mut key_state = Vec::with_capacity(bytes.len());
                                    key_state.extend_from_slice(bytes);
                                    key_state
                                };
                                let bits = UserPtr::<u8>::from(arg).get_as_mut_slice(size)?;
                                return Ok(copy_bytes(&key_state, bits));
                            }
                            // EVIOCGLED
                            0x19 => {
                                return return_zero_bits(arg, size, EventType::Led.bits_count());
                            }
                            // EVIOCGSND
                            0x1a => {
                                return return_zero_bits(arg, size, EventType::Sound.bits_count());
                            }
                            // EVIOCGSW
                            0x1b => {
                                return return_zero_bits(arg, size, EventType::Switch.bits_count());
                            }
                            _ => {}
                        }
                        if nr & !EventType::MAX == EventType::COUNT {
                            return self.get_event_bits(arg, size, nr & EventType::MAX);
                        }
                        const ABS_CNT: u8 = 0x40;
                        if nr & !(ABS_CNT - 1) == ABS_CNT {
                            // EVIOCGABS(axis) — absolute axis info.
                            // libinput needs min/max/res to map the
                            // virtio-tablet's 0..0x7FFF absolute range to
                            // screen pixels; without it motion is treated
                            // as noise.
                            if size < size_of::<InputAbsInfo>() {
                                return Err(AxError::InvalidInput);
                            }
                            let axis = nr & (ABS_CNT - 1);
                            // Linux's evdev returns EINVAL for any axis the
                            // device does not advertise in its EV_ABS bitmap.
                            // virtio-drivers surfaces the same as Error::IoError
                            // (size==0 selector), so without this pre-check
                            // userspace would see EIO and reject the device.
                            if !self.axis_supported(axis) {
                                return Err(AxError::InvalidInput);
                            }
                            let info = match self.inner.lock().device.get_abs_info(axis) {
                                Ok(info) => info,
                                Err(err) => return Err(input_error_to_ax_error(err)),
                            };
                            let abs = InputAbsInfo {
                                value: 0,
                                minimum: info.min,
                                maximum: info.max,
                                fuzz: info.fuzz,
                                flat: info.flat,
                                resolution: info.res,
                            };
                            let bytes = abs.as_bytes();
                            let slice = UserPtr::<u8>::from(arg).get_as_mut_slice(size)?;
                            slice[..bytes.len()].copy_from_slice(bytes);
                            return Ok(bytes.len());
                        }
                        return Err(AxError::InvalidInput);
                    }
                    _ => {}
                }

                Err(AxError::InvalidInput)
            }
        }
    }
}

impl Pollable for EventDev {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, self.inner.lock().has_event());
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if !events.contains(IoEvents::IN) {
            return;
        }
        unsafe { self.waiters.register(context.waker(), IoEvents::IN) };
        if self.inner.lock().has_event() {
            context.waker().wake_by_ref();
        }
    }
}

pub fn input_devices(fs: Arc<SimpleFs>) -> DirMapping {
    let mut inputs = DirMapping::new();
    let mut mice_alias: Option<Arc<EventDev>> = None;
    let mut input_id: u32 = 0;
    let input_devices = ax_input::take_inputs();
    for mut device in input_devices.into_iter() {
        let mut keys = [0; 0x300usize.div_ceil(8)];
        assert!(device.get_event_bits(EventType::Key, &mut keys).unwrap());

        const BTN_MOUSE: usize = 0x110;
        let is_mouse = keys[BTN_MOUSE / 8] & (1 << (BTN_MOUSE % 8)) != 0;

        let event_dev = Arc::new(EventDev::new(device));
        event_dev.register_irq();
        let dev = Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(13, 64 + input_id),
            event_dev.clone(),
        );
        inputs.add(format!("event{input_id}"), dev);
        input_id += 1;

        if is_mouse && mice_alias.is_none() {
            mice_alias = Some(event_dev);
        }
    }

    if let Some(event_dev) = mice_alias {
        inputs.add(
            "mice",
            Device::new(
                fs,
                NodeType::CharacterDevice,
                DeviceId::new(13, 63),
                event_dev,
            ),
        );
    }

    EVENT_DEVICE_COUNT.store(input_id, Ordering::Release);
    inputs
}

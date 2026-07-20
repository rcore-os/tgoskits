use alloc::{format, sync::Arc, vec, vec::Vec};
use core::{
    any::Any,
    mem::offset_of,
    sync::atomic::{AtomicU32, Ordering},
    task::Context,
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
use ax_input::{EventType, InputDeviceFacade, InputDeviceId, InputError};
use ax_runtime::hal::time::wall_time;
use ax_sync::spin::SpinNoIrq as Mutex;
use axfs_ng_vfs::{DeviceId, NodeFlags, NodeType, VfsResult};
use axpoll::{IoEvents, Pollable};
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
    device: InputDeviceFacade,
    key_state: Mutex<Bitmap<KEY_CNT>>,
    prop_bits: [u8; INPUT_PROP_CNT.div_ceil(8)],
}

impl EventDev {
    fn new(device: InputDeviceFacade) -> Self {
        let snapshot = device.snapshot();
        let mut prop_bits = [0; INPUT_PROP_CNT.div_ceil(8)];
        let source = snapshot.property_bits();
        let len = source.len().min(prop_bits.len());
        prop_bits[..len].copy_from_slice(&source[..len]);
        let is_touchscreen = prop_bits[INPUT_PROP_DIRECT / 8] & (1 << (INPUT_PROP_DIRECT % 8)) != 0;
        let has_axes = snapshot.supports_event(EventType::Relative)
            || snapshot.supports_event(EventType::Absolute);
        if has_axes && !is_touchscreen {
            prop_bits[INPUT_PROP_POINTER / 8] |= 1 << (INPUT_PROP_POINTER % 8);
        }
        Self {
            device,
            key_state: Mutex::new(Bitmap::new()),
            prop_bits,
        }
    }

    /// Requests the linear maintenance close transaction.
    pub fn shutdown(&self) -> AxResult<()> {
        self.device
            .request_shutdown()
            .map_err(|_| AxError::BadState)
    }

    fn axis_supported(&self, axis: u8) -> bool {
        (axis as usize) < ABS_MAX && self.device.snapshot().absolute_info(axis).is_some()
    }

    fn get_event_bits(&self, arg: usize, size: usize, ty: u8) -> AxResult<usize> {
        if ty == 0 {
            let mut event_types = [0; (EventType::COUNT as usize).div_ceil(8)];
            for index in 0..EventType::COUNT {
                let Some(event_type) = EventType::from_repr(index) else {
                    continue;
                };
                if self.device.snapshot().supports_event(event_type) {
                    event_types[index as usize / 8] |= 1 << (index % 8);
                }
            }
            return write_user_bytes(arg, size, &event_types);
        }
        let ty = EventType::from_repr(ty).ok_or(AxError::InvalidInput)?;
        write_user_bytes(arg, size, self.device.snapshot().event_bits(ty))
    }
}

impl Drop for EventDev {
    fn drop(&mut self) {
        // Drop only asks the fixed owner to run its explicit close protocol;
        // it never tears down an IRQ action or device source on this thread.
        let _ = self.shutdown();
    }
}

fn write_user_bytes(arg: usize, capacity: usize, source: &[u8]) -> AxResult<usize> {
    let len = source.len().min(capacity);
    UserPtr::<u8>::from(arg).write_slice(&source[..len])?;
    Ok(len)
}

fn return_str(arg: usize, size: usize, s: &str) -> AxResult<usize> {
    write_user_bytes(arg, size, s.as_bytes())
}

fn return_zero_bits(arg: usize, size: usize, bits: usize) -> AxResult<usize> {
    let len = bits.div_ceil(8).min(size);
    UserPtr::<u8>::from(arg).write_slice(&vec![0; len])?;
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
        for out in buf.chunks_exact_mut(size_of::<InputEvent>()) {
            let event = match self.device.read_event() {
                Ok(event) => event,
                Err(InputError::Again) => break,
                Err(error) => {
                    warn!("input facade read failed: {error}");
                    return if read == 0 {
                        Err(AxError::Io)
                    } else {
                        Ok(read)
                    };
                }
            };
            if event.event_type == EventType::Key as u16 && (event.code as usize) < KEY_CNT {
                self.key_state
                    .lock()
                    .set(event.code as usize, event.value != 0);
            }
            let time = wall_time();
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
                UserPtr::<u32>::from(arg).write(0x10001)?;
                Ok(0)
            }
            EVIOCGID => {
                let device_id = self.device.snapshot().device_id();
                let user = UserPtr::<InputDeviceId>::from(arg);
                user.write_field(offset_of!(InputDeviceId, bus_type), device_id.bus_type)?;
                user.write_field(offset_of!(InputDeviceId, vendor), device_id.vendor)?;
                user.write_field(offset_of!(InputDeviceId, product), device_id.product)?;
                user.write_field(offset_of!(InputDeviceId, version), device_id.version)?;
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
                                return return_str(arg, size, self.device.snapshot().name());
                            }
                            // EVIOCGPHYS
                            0x07 => {
                                return return_str(
                                    arg,
                                    size,
                                    self.device.snapshot().physical_location(),
                                );
                            }
                            // EVIOCGUNIQ
                            0x08 => {
                                return return_str(arg, size, self.device.snapshot().unique_id());
                            }
                            // EVIOCGPROP — device property bitmap. libinput
                            // uses INPUT_PROP_POINTER to keep the cursor
                            // visible on absolute-axis pointing devices like
                            // virtio-tablet; we synthesize the bit at probe
                            // for any non-touchscreen with REL/ABS axes.
                            0x09 => {
                                return write_user_bytes(arg, size, &self.prop_bits);
                            }
                            // EVIOCGKEY
                            0x18 => {
                                let key_state = {
                                    let key_state = self.key_state.lock();
                                    let bytes = key_state.as_bytes();
                                    let mut key_state = Vec::with_capacity(bytes.len());
                                    key_state.extend_from_slice(bytes);
                                    key_state
                                };
                                return write_user_bytes(arg, size, &key_state);
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
                            let info = self
                                .device
                                .snapshot()
                                .absolute_info(axis)
                                .ok_or(AxError::InvalidInput)?;
                            let abs = InputAbsInfo {
                                value: 0,
                                minimum: info.min,
                                maximum: info.max,
                                fuzz: info.fuzz,
                                flat: info.flat,
                                resolution: info.res,
                            };
                            let bytes = abs.as_bytes();
                            UserPtr::<u8>::from(arg).write_slice(bytes)?;
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
        events.set(IoEvents::IN, self.device.has_events());
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if !events.contains(IoEvents::IN) {
            return;
        }
        self.device.register_read_waker(context.waker());
    }
}

pub fn input_devices(fs: Arc<SimpleFs>) -> DirMapping {
    let mut inputs = DirMapping::new();
    let mut mice_alias: Option<Arc<EventDev>> = None;
    let mut input_id: u32 = 0;
    let input_devices = ax_input::take_inputs();
    for device in input_devices {
        let keys = device.snapshot().event_bits(EventType::Key);

        const BTN_MOUSE: usize = 0x110;
        let is_mouse = keys
            .get(BTN_MOUSE / 8)
            .is_some_and(|byte| byte & (1 << (BTN_MOUSE % 8)) != 0);

        let event_dev = Arc::new(EventDev::new(device));
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

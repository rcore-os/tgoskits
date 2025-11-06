use alloc::{format, sync::Arc};
use core::{any::Any, task::Context, time::Duration};

#[allow(unused_imports)]
use axdriver::prelude::{
    AxInputDevice, BaseDriverOps, DevError, Event, EventType, InputDeviceId, InputDriverOps,
};
use axerrno::{AxError, AxResult};
use axfs_ng_vfs::{DeviceId, NodeFlags, NodeType, VfsResult};
use axhal::time::wall_time;
use axpoll::{IoEvents, Pollable};
use axsync::Mutex;
use bitmaps::Bitmap;
use linux_raw_sys::{
    general::{__kernel_old_time_t, __kernel_suseconds_t},
    ioctl::{EVIOCGID, EVIOCGRAB, EVIOCGVERSION},
};
use starry_core::vfs::{Device, DeviceOps, DirMapping, SimpleFs};
use zerocopy::{FromBytes, Immutable, IntoBytes};

use crate::mm::UserPtr;
const KEY_CNT: usize = EventType::Key.bits_count();

struct Inner {
    device: AxInputDevice,
    read_ahead: Option<(Duration, Event)>,
    key_state: Bitmap<KEY_CNT>,
}
impl Inner {
    fn has_event(&mut self) -> bool {
        if self.read_ahead.is_none() {
            match self.device.read_event() {
                Ok(event) => {
                    if event.event_type == EventType::Key as u16 {
                        if event.value == 0 {
                            self.key_state.set(event.code as usize, false);
                        } else if event.value == 1 {
                            self.key_state.set(event.code as usize, true);
                        }
                    }
                    self.read_ahead = Some((wall_time(), event));
                }
                Err(DevError::Again) => {}
                Err(err) => {
                    warn!("Failed to read event: {err:?}");
                }
            }
        }
        self.read_ahead.is_some()
    }
}

pub struct EventDev {
    inner: Mutex<Inner>,
    ev_bits: Bitmap<{ EventType::COUNT as usize }>,
}

impl EventDev {
    pub fn new(mut device: AxInputDevice) -> Self {
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

        // let mut out = [0u8; 2000];
        // if device.get_event_bits(EventType::Absolute, &mut out).unwrap() {
        //     let mut bits = Vec::new();
        //     for i in 0..EventType::Absolute.bits_count() {
        //         if (out[i / 8] >> (i % 8)) & 1 != 0 {
        //             bits.push(i);
        //         }
        //     }
        //     warn!("{bits:?}");
        // } else {
        //     warn!("failure");
        // }
        Self {
            inner: Mutex::new(Inner {
                device,
                read_ahead: None,
                key_state: Bitmap::new(),
            }),
            ev_bits,
        }
    }

    fn get_event_bits(&self, arg: usize, size: usize, ty: u8) -> AxResult<usize> {
        let bits = UserPtr::<u8>::from(arg).get_as_mut_slice(size)?;
        if ty == 0 {
            Ok(copy_bytes(self.ev_bits.as_bytes(), bits))
        } else {
            let ty = EventType::from_repr(ty).ok_or(AxError::InvalidInput)?;
            match self.inner.lock().device.get_event_bits(ty, bits) {
                Ok(true) => {}
                Ok(false) => {
                    debug!("No events for {ty:?}");
                }
                Err(err) => {
                    warn!("Failed to get event bits: {err:?}");
                }
            }
            Ok(bits.len().min(ty.bits_count().div_ceil(8)))
        }
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
        for out in buf.chunks_exact_mut(size_of::<InputEvent>()) {
            if !inner.has_event() {
                break;
            }
            let Some((time, event)) = inner.read_ahead.take() else {
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
                *UserPtr::<InputDeviceId>::from(arg).get_as_mut()? =
                    self.inner.lock().device.device_id();
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
                                return return_str(
                                    arg,
                                    size,
                                    self.inner.lock().device.device_name(),
                                );
                            }
                            // EVIOCGPHYS
                            0x07 => {
                                return return_str(
                                    arg,
                                    size,
                                    self.inner.lock().device.physical_location(),
                                );
                            }
                            // EVIOCGUNIQ
                            0x08 => {
                                return return_str(arg, size, self.inner.lock().device.unique_id());
                            }
                            // EVIOCGPROP
                            0x09 => {
                                // For some reasons virtio does not provide prop
                                // bits for now
                                return Ok(0);
                            }
                            // EVIOCGKEY
                            0x18 => {
                                let bits = UserPtr::<u8>::from(arg).get_as_mut_slice(size)?;
                                return Ok(copy_bytes(
                                    self.inner.lock().key_state.as_bytes(),
                                    bits,
                                ));
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
                            // TODO: abs info
                            return Ok(0);
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
        if events.contains(IoEvents::IN) {
            context.waker().wake_by_ref();
        }
    }
}

pub fn input_devices(fs: Arc<SimpleFs>) -> DirMapping {
    let mut inputs = DirMapping::new();
    let mut input_id = 0;
    let input_devices = axinput::take_inputs();
    let mut keys = [0; 0x300usize.div_ceil(8)];
    for (i, mut device) in input_devices.into_iter().enumerate() {
        assert!(device.get_event_bits(EventType::Key, &mut keys).unwrap());

        let dev = Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(13, (i + 1) as _),
            Arc::new(EventDev::new(device)),
        );

        const BTN_MOUSE: usize = 0x110;
        if keys[BTN_MOUSE / 8] & (1 << (BTN_MOUSE % 8)) != 0 {
            // Mouse
            inputs.add("mice", dev);
        } else {
            inputs.add(format!("event{input_id}"), dev);
            input_id += 1;
        }
    }
    inputs
}

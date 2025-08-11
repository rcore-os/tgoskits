//! Special devices

mod event;
mod fb;
mod r#loop;
mod rtc;
mod tty;

use alloc::{format, sync::Arc};
use core::any::Any;

use axdriver::prelude::EventType;
use axerrno::{LinuxError, LinuxResult};
use axfs_ng::FsContext;
use axfs_ng_vfs::{DeviceId, Filesystem, NodeFlags, NodeType, VfsResult};
use axsync::Mutex;
use rand::{RngCore, SeedableRng, rngs::SmallRng};
use starry_core::vfs::{Device, DeviceMmap, DeviceOps, DirMaker, DirMapping, SimpleDir, SimpleFs};
pub use tty::N_TTY;

const RANDOM_SEED: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

pub(crate) fn new_devfs() -> LinuxResult<Filesystem> {
    let fs = SimpleFs::new_with("devfs".into(), 0x01021994, builder);
    let mp = axfs_ng_vfs::Mountpoint::new_root(&fs);
    FsContext::new(mp.root_location())
        .resolve("/shm")?
        .mount(&super::tmp::MemoryFs::new())?;
    Ok(fs)
}

struct Null;

impl DeviceOps for Null {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

struct Zero;

impl DeviceOps for Zero {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        buf.fill(0);
        Ok(buf.len())
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn mmap(&self) -> DeviceMmap {
        DeviceMmap::ReadOnly
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

struct Random {
    rng: Mutex<SmallRng>,
}

impl Random {
    pub fn new() -> Self {
        Self {
            rng: Mutex::new(SmallRng::from_seed(*RANDOM_SEED)),
        }
    }
}

impl DeviceOps for Random {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        self.rng.lock().fill_bytes(buf);
        Ok(buf.len())
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

struct Full;

impl DeviceOps for Full {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        buf.fill(0);
        Ok(buf.len())
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(LinuxError::ENOSPC)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

fn builder(fs: Arc<SimpleFs>) -> DirMaker {
    let mut root = DirMapping::new();
    root.add(
        "null",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 3),
            Arc::new(Null),
        ),
    );
    root.add(
        "zero",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 5),
            Arc::new(Zero),
        ),
    );
    root.add(
        "full",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 7),
            Arc::new(Full),
        ),
    );
    root.add(
        "random",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 8),
            Arc::new(Random::new()),
        ),
    );
    root.add(
        "urandom",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 9),
            Arc::new(Random::new()),
        ),
    );
    root.add(
        "rtc0",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            rtc::RTC0_DEVICE_ID,
            Arc::new(rtc::Rtc),
        ),
    );
    if axdisplay::has_display() {
        root.add(
            "fb0",
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                DeviceId::new(29, 0),
                Arc::new(fb::FrameBuffer::new()),
            ),
        );
    }

    let tty = Device::new(
        fs.clone(),
        NodeType::CharacterDevice,
        DeviceId::new(5, 0),
        N_TTY.clone(),
    );
    root.add("tty", tty.clone());
    root.add("console", tty.clone());

    #[cfg(feature = "memtrack")]
    root.add(
        "memtrack",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(114, 514),
            Arc::new(memtrack::MemTrack),
        ),
    );

    // This is mounted to a tmpfs in `new_procfs`
    root.add(
        "shm",
        SimpleDir::new_maker(fs.clone(), Arc::new(DirMapping::new())),
    );

    // Loop devices
    for i in 0..16 {
        let dev_id = DeviceId::new(7, 0);
        root.add(
            format!("loop{i}"),
            Device::new(
                fs.clone(),
                NodeType::BlockDevice,
                dev_id,
                Arc::new(r#loop::LoopDevice::new(i, dev_id)),
            ),
        );
    }

    // Input devices
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
            Arc::new(event::EventDev::new(device)),
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
    root.add("input", SimpleDir::new_maker(fs.clone(), Arc::new(inputs)));

    SimpleDir::new_maker(fs, Arc::new(root))
}

#[cfg(feature = "memtrack")]
mod memtrack;

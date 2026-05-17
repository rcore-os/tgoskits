//! Special devices

#[cfg(all(feature = "rknpu", not(any(windows, unix))))]
mod card0;
#[cfg(all(feature = "rknpu", not(any(windows, unix))))]
mod card1;
#[cfg(all(feature = "rknpu", not(any(windows, unix))))]
mod dma_heap;
#[cfg(all(feature = "rknpu", not(any(windows, unix))))]
mod drm;
#[cfg(feature = "input")]
mod event;
mod fb;
#[cfg(feature = "dev-log")]
mod log;
mod r#loop;
#[cfg(feature = "memtrack")]
mod memtrack;
pub mod tty;
mod tty_serial;

use alloc::{format, sync::Arc};
use core::any::Any;

use ax_errno::AxError;
use ax_sync::Mutex;
use axfs_ng_vfs::{DeviceId, Filesystem, NodeFlags, NodeType, VfsResult};
#[cfg(feature = "dev-log")]
pub use log::bind_dev_log;
use rand::{Rng, SeedableRng, rngs::SmallRng};

use crate::pseudofs::{Device, DeviceOps, DirMaker, DirMapping, SimpleDir, SimpleFs};

const RANDOM_SEED: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

pub(crate) fn new_devfs() -> Filesystem {
    SimpleFs::new_with("devfs".into(), 0x01021994, builder)
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
        Err(AxError::StorageFull)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

struct CpuDmaLatency;

impl DeviceOps for CpuDmaLatency {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
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
    if ax_display::has_display() {
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

    root.add(
        "tty",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(5, 0),
            Arc::new(tty::CurrentTty),
        ),
    );
    root.add(
        "console",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(5, 1),
            tty::N_TTY.clone(),
        ),
    );

    root.add(
        "ptmx",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(5, 2),
            Arc::new(tty::Ptmx(fs.clone())),
        ),
    );
    root.add(
        "pts",
        SimpleDir::new_maker(fs.clone(), Arc::new(tty::PtsDir)),
    );
    #[cfg(feature = "dev-log")]
    root.add(
        "log",
        crate::pseudofs::SimpleFile::new(fs.clone(), NodeType::Socket, || Ok(b"")),
    );

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

    root.add(
        "cpu_dma_latency",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(10, 1024),
            Arc::new(CpuDmaLatency),
        ),
    );


    // ── Serial TTY透传设备 ─────────────────────────────────────────────────────
    // Linux 标准：major=4，minor=64+n（ttyS0=64, ttyS1=65, ttyS3=67, …）
    //
    // 板卡：OrangePi 5 Plus (RK3588)
    //
    // /dev/ttyS1 → UART1 @ 0xFEB40000, IRQ 364
    //   pinmux uart1m1-xfer:
    //     GPIO1_B7 → UART1_TX  40-pin header Pin  8  → 接对端 RX
    //     GPIO1_B6 → UART1_RX  40-pin header Pin 10  → 接对端 TX
    //     GND                  40-pin header Pin  6
    //
    // /dev/ttyS3 → UART3 @ 0xFEB60000, IRQ 366
    //   pinmux uart3m1-xfer:
    //     GPIO3_B6 → UART3_TX  40-pin header Pin 32  → 接对端 RX
    //     GPIO3_B5 → UART3_RX  40-pin header Pin 33  → 接对端 TX
    //
    // !! UART2 (0xFEB50000) 已被板载 Type-C debug 口占用，不可使用 !!
    let tty_s1 = Arc::new(tty_serial::new_tty_s1(115200));
    root.add(
        "ttyS1",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(4, 65),
            tty_s1,
        ),
    );

    let tty_s3 = Arc::new(tty_serial::new_tty_s3(115200));
    root.add(
        "ttyS3",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(4, 67),
            tty_s3,
        ),
    );

    // This is mounted to a tmpfs in `new_procfs`
    root.add(
        "shm",
        SimpleDir::new_maker(fs.clone(), Arc::new(DirMapping::new())),
    );
    {
        let mut bus_dir = DirMapping::new();
        bus_dir.add(
            "usb",
            SimpleDir::new_maker(fs.clone(), Arc::new(DirMapping::new())),
        );
        root.add("bus", SimpleDir::new_maker(fs.clone(), Arc::new(bus_dir)));
    }

    #[cfg(all(feature = "rknpu", not(any(windows, unix))))]
    {
        // DMA heap devices
        let mut dma_heap_dir = DirMapping::new();
        dma_heap_dir.add(
            "system",
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                dma_heap::DMA_HEAP_SYSTEM_DEVICE_ID,
                Arc::new(dma_heap::DmaHeapSystem::new()),
            ),
        );
        root.add(
            "dma_heap",
            SimpleDir::new_maker(fs.clone(), Arc::new(dma_heap_dir)),
        );

        // DRI devices
        let mut dri_dir = DirMapping::new();
        dri_dir.add(
            "card0",
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                card0::CARD0_SYSTEM_DEVICE_ID,
                Arc::new(card0::Card0::new()),
            ),
        );
        dri_dir.add(
            "card1",
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                card1::CARD1_SYSTEM_DEVICE_ID,
                Arc::new(card1::Card1::new()),
            ),
        );
        root.add("dri", SimpleDir::new_maker(fs.clone(), Arc::new(dri_dir)));
    }

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
    #[cfg(feature = "input")]
    root.add(
        "input",
        SimpleDir::new_maker(fs.clone(), Arc::new(event::input_devices(fs.clone()))),
    );

    SimpleDir::new_maker(fs, Arc::new(root))
}

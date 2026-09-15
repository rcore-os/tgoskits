//! sysfs — a minimal `/sys` tree shaped for `libudev` enumeration.
//!
//! `libudev_enumerate_scan_devices` walks `/sys/class/<subsystem>/<name>`,
//! then calls `realpath()` on each entry and uses `dirname()` on the
//! result to find the parent device.  That requires each `/sys/class/`
//! entry to be a symlink into `/sys/devices/...` — if the entry is a
//! real directory, `realpath()` stays inside `/sys/class/` and
//! `dirname()` yields the subsystem container, which has no `uevent`
//! and therefore produces an unusable device record.
//!
//! Layout:
//!   - Real device dirs live under `/sys/devices/virtual/<subsystem>/...`.
//!   - `/sys/class/<subsystem>/<name>` are symlinks to the real dirs.
//!   - `/sys/dev/char/<maj>:<min>` symlinks let libudev resolve a fd's
//!     `fstat().st_rdev` to a syspath.  libinput's
//!     `evdev_device_have_same_syspath` requires this.
//!   - `/sys/devices/platform/...` hosts a parent-bus stub so the
//!     `device` symlink from a virtual device has somewhere to resolve to.
//!     Mesa's DRI loader reads PCI vendor/device files from here.
//!
//! Out of scope (deliberately):
//!   - Writeable sysfs knobs (`/sys/kernel/*`, `/sys/module/*`).
//!   - Dynamic uevent emission via `/sys/.../uevent` writes (depends on
//!     AF_NETLINK broadcast and is not implemented here).
//!   - ALSA `sound/` subsystem (separate submission).

use alloc::{
    borrow::{Cow, ToOwned},
    boxed::Box,
    format,
    string::String,
    sync::Arc,
    vec::Vec,
};

use axfs_ng_vfs::{Filesystem, NodeType, VfsError, VfsResult};

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "loongarch64"))]
pub use self::cache::init_cpu_cache;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "loongarch64"))]
use self::cache::{CpuCacheDir, has_cache_leaves};
use crate::pseudofs::{
    DirMaker, DirMapping, NodeOpsMux, SimpleDir, SimpleDirOps, SimpleFile, SimpleFs,
};

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "loongarch64"))]
mod cache;

/// RISC-V describes caches only in the device tree, so no CPU exposes `cache/`.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "loongarch64")))]
pub fn init_cpu_cache() {}

/// The DRM major number. Matches Linux's DRM_MAJOR (226).
const DRM_MAJOR: u32 = 226;
/// Framebuffer major. Matches Linux's FB_MAJOR (29).
const FB_MAJOR: u32 = 29;
/// Input-event major. Matches Linux's INPUT_MAJOR (13).
const INPUT_MAJOR: u32 = 13;
/// First minor number for `/dev/input/event*`. Matches Linux's
/// `EVDEV_MINOR_BASE`.
const EVDEV_MINOR_BASE: u32 = 64;

/// Standard libinput-consumable evdev tag set. We over-tag rather than
/// classify per device — libinput cross-references real evdev capabilities
/// via `EVIOCGBIT` and only exposes a device with the appropriate role
/// once those bits match. Linux's `60-input-id.rules` produces these at
/// udevd startup; we don't run udevd.
const EVDEV_TAGS: &[&str] = &["ID_INPUT", "ID_INPUT_KEYBOARD", "ID_INPUT_MOUSE"];

/// Build the sysfs filesystem.
pub fn new_sysfs() -> Filesystem {
    // 0x62656572 = sysfs magic.
    SimpleFs::new_with("sysfs".into(), 0x62656572, builder)
}

fn builder(fs: Arc<SimpleFs>) -> DirMaker {
    let mut root = DirMapping::new();
    root.add(
        "class",
        SimpleDir::new_maker(fs.clone(), Arc::new(ClassDir { fs: fs.clone() })),
    );
    root.add(
        "bus",
        SimpleDir::new_maker(fs.clone(), Arc::new(BusDir { fs: fs.clone() })),
    );
    root.add(
        "devices",
        SimpleDir::new_maker(fs.clone(), Arc::new(DevicesDir { fs: fs.clone() })),
    );
    // /sys/dev/{char,block}/<major>:<minor> — symlinks to the real
    // device dirs under /sys/devices/.  libudev's
    // udev_device_new_from_devnum() uses these to map a (char, major,
    // minor) tuple back to a sysfs path.  libinput calls that function
    // when verifying an fd and its udev device refer to the same node.
    root.add(
        "dev",
        SimpleDir::new_maker(fs.clone(), Arc::new(DevDir { fs: fs.clone() })),
    );
    root.add("kernel", {
        let mut kernel = DirMapping::new();
        kernel.add(
            "debug",
            SimpleDir::new_maker(fs.clone(), Arc::new(DirMapping::new())),
        );
        SimpleDir::new_maker(fs.clone(), Arc::new(kernel))
    });
    // `/sys/fs/cgroup` is the mount point systemd lays its cgroup hierarchy on
    // (it mounts tmpfs then cgroup2 here). On Linux the kernel provides this
    // empty directory inside sysfs; once sysfs is mounted over /sys it shadows
    // the rootfs's own /sys/fs/cgroup, so the mount point must exist here or
    // `mount("/sys/fs/cgroup")` fails with ENOENT.
    root.add("fs", {
        let mut fs_dir = DirMapping::new();
        fs_dir.add(
            "cgroup",
            SimpleDir::new_maker(fs.clone(), Arc::new(DirMapping::new())),
        );
        SimpleDir::new_maker(fs.clone(), Arc::new(fs_dir))
    });
    SimpleDir::new_maker(fs.clone(), Arc::new(root))
}

/// `/sys/dev/` — `char/` and `block/` subdirs.
struct DevDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for DevDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(["char", "block"].into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(NodeOpsMux::Dir(match name {
            "char" => SimpleDir::new_maker(fs.clone(), Arc::new(DevCharDir { fs })),
            // No block devices yet — present as empty rather than 404
            // so libudev's "enumerate all block" doesn't bail.
            "block" => SimpleDir::new_maker(fs.clone(), Arc::new(DevBlockDir)),
            _ => return Err(VfsError::NotFound),
        }))
    }
}

/// `/sys/dev/block/` — empty placeholder.
struct DevBlockDir;

impl SimpleDirOps for DevBlockDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(core::iter::empty())
    }

    fn lookup_child(&self, _name: &str) -> VfsResult<NodeOpsMux> {
        Err(VfsError::NotFound)
    }
}

/// `/sys/dev/char/<major>:<minor>` — symlinks to the real device dirs.
struct DevCharDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for DevCharDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let mut v: Vec<Cow<'a, str>> = alloc::vec![
            Cow::Owned(format!("{DRM_MAJOR}:0")),
            Cow::Owned(format!("{FB_MAJOR}:0")),
        ];
        for i in 0..input_device_count() {
            v.push(Cow::Owned(format!(
                "{INPUT_MAJOR}:{}",
                EVDEV_MINOR_BASE + i
            )));
        }
        Box::new(v.into_iter())
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let (maj, min) = name
            .split_once(':')
            .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)))
            .ok_or(VfsError::NotFound)?;
        let target = match (maj, min) {
            (DRM_MAJOR, 0) => "../../devices/virtual/drm/card0".to_owned(),
            (FB_MAJOR, 0) => "../../devices/virtual/graphics/fb0".to_owned(),
            (INPUT_MAJOR, m)
                if m >= EVDEV_MINOR_BASE && (m - EVDEV_MINOR_BASE) < input_device_count() =>
            {
                let n = m - EVDEV_MINOR_BASE;
                format!("../../devices/virtual/input/input{n}/event{n}")
            }
            _ => return Err(VfsError::NotFound),
        };
        Ok(
            SimpleFile::new(self.fs.clone(), NodeType::Symlink, move || {
                Ok(target.clone())
            })
            .into(),
        )
    }
}

// ========================================================================
// /sys/class
// ========================================================================

struct ClassDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for ClassDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        #[cfg(any(feature = "sg2002", feature = "rk3588-pwm"))]
        let names: &'static [&'static str] = &["drm", "graphics", "input", "pwm"];
        #[cfg(not(any(feature = "sg2002", feature = "rk3588-pwm")))]
        let names: &'static [&'static str] = &["drm", "graphics", "input"];
        Box::new(names.iter().copied().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(NodeOpsMux::Dir(match name {
            "drm" => SimpleDir::new_maker(
                fs.clone(),
                Arc::new(ClassSubsystemDir::new(fs, "drm", &["card0"])),
            ),
            "graphics" => SimpleDir::new_maker(
                fs.clone(),
                Arc::new(ClassSubsystemDir::new(fs, "graphics", &["fb0"])),
            ),
            "input" => SimpleDir::new_maker(fs.clone(), Arc::new(InputClassDir { fs })),
            #[cfg(any(feature = "sg2002", feature = "rk3588-pwm"))]
            "pwm" => crate::pseudofs::dev::pwm::pwm_class_dir_maker(fs),
            _ => return Err(VfsError::NotFound),
        }))
    }
}

/// `/sys/class/<subsystem>/<name>` — every entry is a symlink into
/// `/sys/devices/virtual/<subsystem>/...`.
struct ClassSubsystemDir {
    fs: Arc<SimpleFs>,
    subsystem: &'static str,
    names: Vec<&'static str>,
}

impl ClassSubsystemDir {
    fn new(fs: Arc<SimpleFs>, subsystem: &'static str, names: &[&'static str]) -> Self {
        Self {
            fs,
            subsystem,
            names: names.to_vec(),
        }
    }
}

impl SimpleDirOps for ClassSubsystemDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(self.names.iter().copied().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        if !self.names.contains(&name) {
            return Err(VfsError::NotFound);
        }
        let target = format!("../../devices/virtual/{}/{}", self.subsystem, name);
        Ok(
            SimpleFile::new(self.fs.clone(), NodeType::Symlink, move || {
                Ok(target.clone())
            })
            .into(),
        )
    }
}

/// `/sys/class/input/event<N>` — symlinks based on how many evdev devices
/// are registered.  Each points at
/// `/sys/devices/virtual/input/input<N>/event<N>` so libinput's walk
/// up through the parent `input<N>` container resolves correctly.
struct InputClassDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for InputClassDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let names: Vec<_> = (0..input_device_count())
            .map(|i| Cow::Owned(format!("event{i}")))
            .collect();
        Box::new(names.into_iter())
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let n = name
            .strip_prefix("event")
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or(VfsError::NotFound)?;
        if n >= input_device_count() {
            return Err(VfsError::NotFound);
        }
        let target = format!("../../devices/virtual/input/input{n}/event{n}");
        Ok(
            SimpleFile::new(self.fs.clone(), NodeType::Symlink, move || {
                Ok(target.clone())
            })
            .into(),
        )
    }
}

#[cfg(feature = "input")]
fn input_device_count() -> u32 {
    crate::pseudofs::dev::event::input_device_count()
}

#[cfg(not(feature = "input"))]
fn input_device_count() -> u32 {
    0
}

// ========================================================================
// /sys/bus
// ========================================================================

struct BusDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for BusDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let names: &'static [&'static str] = if crate::pseudofs::usbfs::has_manager() {
            &["platform", "usb", "event_source"]
        } else {
            &["platform", "event_source"]
        };
        Box::new(names.iter().copied().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(NodeOpsMux::Dir(match name {
            "platform" => SimpleDir::new_maker(fs.clone(), Arc::new(PlatformBusClassDir)),
            "event_source" => {
                SimpleDir::new_maker(fs.clone(), Arc::new(EventSourceBusDir { fs: fs.clone() }))
            }
            "usb" if crate::pseudofs::usbfs::has_manager() => {
                SimpleDir::new_maker(fs.clone(), Arc::new(DirMapping::new()))
            }
            _ => return Err(VfsError::NotFound),
        }))
    }
}

struct PlatformBusClassDir;

impl SimpleDirOps for PlatformBusClassDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(core::iter::empty())
    }

    fn lookup_child(&self, _name: &str) -> VfsResult<NodeOpsMux> {
        Err(VfsError::NotFound)
    }
}

// /sys/bus/event_source/devices/<source>/type — aya reads this to learn the
// dynamic perf_event_type for each event source (kprobe / uprobe / tracepoint).
// Values match `kbpf_basic::perf::PerfTypeId` so the user-supplied number
// dispatches cleanly in `perf_event_open`.
const PERF_EVENT_SOURCES: &[(&str, u32)] = &[
    ("software", 1),   // PERF_TYPE_SOFTWARE
    ("kprobe", 6),     // PerfTypeId::PERF_TYPE_KPROBE
    ("uprobe", 7),     // PerfTypeId::PERF_TYPE_UPROBE
    ("tracepoint", 2), // PERF_TYPE_TRACEPOINT
];

/// The hardware PMU device name advertised under
/// `/sys/bus/event_source/devices/`. The real `perf` tool reads
/// `<this>/type` to learn the dynamic perf type and resolves named events such
/// as `armv8_pmuv3_0/cpu_cycles/` against `<this>/events/` + `<this>/format/`.
/// Only meaningful on aarch64 (ARM PMUv3).
#[cfg(target_arch = "aarch64")]
const ARMV8_PMUV3_DEVICE: &str = "armv8_pmuv3_0";
#[cfg(target_arch = "aarch64")]
const ARMV8_CORTEX_A55_DEVICE: &str = "armv8_cortex_a55";
#[cfg(target_arch = "aarch64")]
const ARMV8_CORTEX_A76_DEVICE: &str = "armv8_cortex_a76";

/// Named ARM PMUv3 event aliases exposed under
/// `/sys/bus/event_source/devices/armv8_pmuv3_0/events/<name>`, each serving
/// `"event=0xNN\n"`. `perf` substitutes the parsed value into the `config` bits
/// declared by `format/event` (`config:0-15`). These are the standard ARMv8
/// PMUv3 event numbers (ARM ARM, `PMU events`). `cpu_cycles` (ARM event `0x11`)
/// is the primary/default event.
#[cfg(target_arch = "aarch64")]
const ARMV8_PMUV3_EVENTS: &[(&str, u16)] = &[
    ("cpu_cycles", 0x11),
    ("instructions", 0x08),
    ("cache_references", 0x04),
    ("cache_misses", 0x03),
    ("l1d_cache", 0x04),
    ("l1d_cache_refill", 0x03),
    ("l1i_cache_refill", 0x01),
    ("branch_instructions", 0x21),
    ("branch_misses", 0x10),
    ("bus_cycles", 0x1d),
    ("br_retired", 0x21),
    ("br_mis_pred", 0x10),
    ("inst_retired", 0x08),
];

struct EventSourceBusDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for EventSourceBusDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(["devices"].into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(NodeOpsMux::Dir(match name {
            "devices" => SimpleDir::new_maker(
                fs.clone(),
                Arc::new(EventSourceDevicesDir { fs: fs.clone() }),
            ),
            _ => return Err(VfsError::NotFound),
        }))
    }
}

struct EventSourceDevicesDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for EventSourceDevicesDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let devices: Vec<Cow<'a, str>> = PERF_EVENT_SOURCES
            .iter()
            .map(|(name, _)| Cow::Borrowed(*name))
            .collect();
        #[cfg(target_arch = "aarch64")]
        let mut devices = devices;
        #[cfg(target_arch = "aarch64")]
        {
            use crate::perf::event_map::ClusterId;

            if crate::perf::percpu::has_pmu() {
                devices.push(Cow::Borrowed(ARMV8_PMUV3_DEVICE));
            }
            if crate::perf::percpu::has_cluster(ClusterId::CortexA55) {
                devices.push(Cow::Borrowed(ARMV8_CORTEX_A55_DEVICE));
            }
            if crate::perf::percpu::has_cluster(ClusterId::CortexA76) {
                devices.push(Cow::Borrowed(ARMV8_CORTEX_A76_DEVICE));
            }
        }
        Box::new(devices.into_iter())
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        #[cfg(target_arch = "aarch64")]
        if let Some((ty, cluster)) = hw_pmu_source(name) {
            return Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                fs.clone(),
                Arc::new(HwPmuDeviceDir { fs, ty, cluster }),
            )));
        }
        let ty = PERF_EVENT_SOURCES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, t)| *t)
            .ok_or(VfsError::NotFound)?;
        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            fs.clone(),
            Arc::new(EventSourceDeviceDir { fs: fs.clone(), ty }),
        )))
    }
}

/// `/sys/bus/event_source/devices/armv8_pmuv3_0/` — the ARM PMUv3 CPU PMU,
/// the hardware event source the real `perf` tool drives. Richer than the
/// tracing devices: it exposes `type`, `cpus`, a `format/` describing where the
/// event number lives in `config`, and an `events/` directory of named event
/// aliases. aarch64-only.
#[cfg(target_arch = "aarch64")]
struct HwPmuDeviceDir {
    fs: Arc<SimpleFs>,
    ty: u32,
    cluster: Option<crate::perf::event_map::ClusterId>,
}

#[cfg(target_arch = "aarch64")]
impl SimpleDirOps for HwPmuDeviceDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            ["type", "cpus", "format", "events"]
                .into_iter()
                .map(Cow::Borrowed),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        match name {
            // The dynamic perf type `perf` puts in `perf_event_attr.type`; the
            // dispatcher routes it to the hardware-PMU backend.
            "type" => {
                let body = format!("{}\n", self.ty);
                Ok(SimpleFile::new_regular(fs, move || Ok(body.clone())).into())
            }
            "cpus" => {
                let cluster = self.cluster;
                Ok(
                    SimpleFile::new_regular(fs, move || Ok(crate::perf::percpu::cpu_list(cluster)))
                        .into(),
                )
            }
            "format" => Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                fs.clone(),
                Arc::new(HwPmuFormatDir { fs }),
            ))),
            "events" => Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                fs.clone(),
                Arc::new(HwPmuEventsDir {
                    fs,
                    cluster: self.cluster,
                }),
            ))),
            _ => Err(VfsError::NotFound),
        }
    }
}

/// `/sys/bus/event_source/devices/armv8_pmuv3_0/format/` — declares the bit
/// layout of `perf_event_attr.config`. `perf` parses `event=config:0-15` to
/// learn that the event number it looks up under `events/` belongs in bits
/// `0..=15` of `config`, exactly where [`crate::perf::hw::perf_event_open_hw`]
/// reads it (`config & 0xFFFF`).
#[cfg(target_arch = "aarch64")]
struct HwPmuFormatDir {
    fs: Arc<SimpleFs>,
}

#[cfg(target_arch = "aarch64")]
impl SimpleDirOps for HwPmuFormatDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(["event"].into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        match name {
            "event" => Ok(SimpleFile::new_regular(fs, || Ok("config:0-15\n".to_owned())).into()),
            _ => Err(VfsError::NotFound),
        }
    }
}

/// `/sys/bus/event_source/devices/armv8_pmuv3_0/events/` — named event aliases.
/// Each file `<name>` serves `"event=0xNN\n"`; `perf` substitutes the value
/// into the `config` bits declared by `format/event` to build the
/// `perf_event_attr`. See [`ARMV8_PMUV3_EVENTS`].
#[cfg(target_arch = "aarch64")]
struct HwPmuEventsDir {
    fs: Arc<SimpleFs>,
    cluster: Option<crate::perf::event_map::ClusterId>,
}

#[cfg(target_arch = "aarch64")]
impl SimpleDirOps for HwPmuEventsDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            ARMV8_PMUV3_EVENTS.iter().filter_map(|(name, _)| {
                hw_pmu_alias(self.cluster, name).map(|_| Cow::Borrowed(*name))
            }),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        let event = hw_pmu_alias(self.cluster, name).ok_or(VfsError::NotFound)?;
        let body = format!("event={event:#04x}\n");
        Ok(SimpleFile::new_regular(fs, move || Ok(body.clone())).into())
    }
}

#[cfg(target_arch = "aarch64")]
fn hw_pmu_source(name: &str) -> Option<(u32, Option<crate::perf::event_map::ClusterId>)> {
    use crate::perf::{
        event_map::ClusterId,
        hw::{ARMV8_CORTEX_A55_PERF_TYPE, ARMV8_CORTEX_A76_PERF_TYPE, ARMV8_PMUV3_PERF_TYPE},
    };

    match name {
        ARMV8_PMUV3_DEVICE if crate::perf::percpu::has_pmu() => Some((ARMV8_PMUV3_PERF_TYPE, None)),
        ARMV8_CORTEX_A55_DEVICE if crate::perf::percpu::has_cluster(ClusterId::CortexA55) => {
            Some((ARMV8_CORTEX_A55_PERF_TYPE, Some(ClusterId::CortexA55)))
        }
        ARMV8_CORTEX_A76_DEVICE if crate::perf::percpu::has_cluster(ClusterId::CortexA76) => {
            Some((ARMV8_CORTEX_A76_PERF_TYPE, Some(ClusterId::CortexA76)))
        }
        _ => None,
    }
}

#[cfg(target_arch = "aarch64")]
fn hw_pmu_alias(cluster: Option<crate::perf::event_map::ClusterId>, name: &str) -> Option<u16> {
    let declared = ARMV8_PMUV3_EVENTS
        .iter()
        .find(|(declared, _)| *declared == name)
        .map(|(_, event)| *event)?;
    let event = if name == "branch_instructions" {
        crate::perf::percpu::branch_event_for(cluster)?
    } else {
        declared
    };
    crate::perf::percpu::event_supported_on(cluster, event).then_some(event)
}

struct EventSourceDeviceDir {
    fs: Arc<SimpleFs>,
    ty: u32,
}

impl EventSourceDeviceDir {
    /// kprobe (6) / uprobe (7) PMUs support a return-probe variant selected via
    /// the `retprobe` config bit; tracepoint (2) does not.
    fn supports_retprobe(&self) -> bool {
        self.ty == 6 || self.ty == 7
    }
}

impl SimpleDirOps for EventSourceDeviceDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        if self.supports_retprobe() {
            Box::new(["type", "format"].into_iter().map(Cow::Borrowed))
        } else {
            Box::new(["type"].into_iter().map(Cow::Borrowed))
        }
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        match name {
            "type" => {
                let body = format!("{}\n", self.ty);
                Ok(SimpleFile::new_regular(fs, move || Ok(body.clone())).into())
            }
            // `/sys/bus/event_source/devices/<k|u>probe/format/` — aya reads
            // `format/retprobe` to learn which `config` bit selects the
            // return-probe variant before `perf_event_open` for a kretprobe /
            // uretprobe.
            "format" if self.supports_retprobe() => Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                fs.clone(),
                Arc::new(EventSourceFormatDir { fs }),
            ))),
            _ => Err(VfsError::NotFound),
        }
    }
}

struct EventSourceFormatDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for EventSourceFormatDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(["retprobe"].into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        match name {
            // `config:0` = the retprobe flag lives in bit 0 of `config`, which
            // is exactly what `perf_event_open_kprobe` decodes (config 1 =
            // kretprobe). Matches the format string the real kernel exposes.
            "retprobe" => Ok(SimpleFile::new_regular(fs, || Ok("config:0\n".to_owned())).into()),
            _ => Err(VfsError::NotFound),
        }
    }
}

// ========================================================================
// /sys/devices
// ========================================================================

struct DevicesDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for DevicesDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        #[cfg(target_arch = "aarch64")]
        let pmu = core::iter::once(Cow::Borrowed(ARMV8_PMUV3_DEVICE));
        #[cfg(not(target_arch = "aarch64"))]
        let pmu = core::iter::empty();
        Box::new(
            ["platform", "system", "virtual"]
                .into_iter()
                .map(Cow::Borrowed)
                .chain(pmu),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(NodeOpsMux::Dir(match name {
            "platform" => SimpleDir::new_maker(fs.clone(), Arc::new(PlatformBusDir { fs })),
            "system" => SimpleDir::new_maker(fs.clone(), Arc::new(SystemDir { fs })),
            "virtual" => SimpleDir::new_maker(fs.clone(), Arc::new(VirtualDir { fs })),
            #[cfg(target_arch = "aarch64")]
            ARMV8_PMUV3_DEVICE => SimpleDir::new_maker(fs.clone(), Arc::new(PmuDeviceDir { fs })),
            _ => return Err(VfsError::NotFound),
        }))
    }
}

/// `/sys/devices/armv8_pmuv3_0/` — the ARM CPU PMU device node.
///
/// `perf record` reads `cpuid` (the raw MIDR_EL1, hex-encoded) to select the
/// right `pmu-events` JSON map for the detected CPU microarchitecture.
#[cfg(target_arch = "aarch64")]
struct PmuDeviceDir {
    fs: Arc<SimpleFs>,
}

#[cfg(target_arch = "aarch64")]
impl SimpleDirOps for PmuDeviceDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(["cpuid"].into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match name {
            "cpuid" => Ok(SimpleFile::new_regular(self.fs.clone(), || {
                // perf reads this to identify the CPU core and select a
                // microarchitectural event map. Format is the raw MIDR_EL1
                // as hex (no 0x prefix) — perf's filename__read_str reads
                // until EOF and compares bytewise.
                let midr = crate::perf::read_midr_el1();
                Ok(alloc::format!("{midr:016x}\n"))
            })
            .into()),
            _ => Err(VfsError::NotFound),
        }
    }
}

/// `/sys/devices/system/` — kernel topology subsystems.
struct SystemDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for SystemDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(["cpu", "node"].into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match name {
            "cpu" => Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                self.fs.clone(),
                Arc::new(SystemCpuDir {
                    fs: self.fs.clone(),
                }),
            ))),
            // `/sys/devices/system/node/` - a single UMA node. hwloc (used by pocl, numactl, ...)
            // reads `nodeN/meminfo`'s `Node N MemTotal:` line to size device global memory; without
            // it hwloc reports 0 and pocl advertises a 0-byte OpenCL device. Linux always exposes
            // this even on non-NUMA machines.
            "node" => Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                self.fs.clone(),
                Arc::new(SystemNodeDir {
                    fs: self.fs.clone(),
                }),
            ))),
            _ => Err(VfsError::NotFound),
        }
    }
}

/// `/sys/devices/system/node/` - one memory node (node0) covering all CPUs and RAM.
struct SystemNodeDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for SystemNodeDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            [
                "online",
                "possible",
                "has_normal_memory",
                "has_cpu",
                "node0",
            ]
            .into_iter()
            .map(Cow::Borrowed),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(match name {
            "online" | "possible" | "has_normal_memory" | "has_cpu" => {
                SimpleFile::new_regular(fs, || Ok("0\n".to_owned())).into()
            }
            "node0" => NodeOpsMux::Dir(SimpleDir::new_maker(
                fs.clone(),
                Arc::new(SystemNodeEntryDir { fs }),
            )),
            _ => return Err(VfsError::NotFound),
        })
    }
}

/// `/sys/devices/system/node/node0/` - the node's memory + CPU map.
struct SystemNodeEntryDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for SystemNodeEntryDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            ["meminfo", "cpumap", "cpulist", "distance"]
                .into_iter()
                .map(Cow::Borrowed),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(match name {
            "meminfo" => SimpleFile::new_regular(fs, || Ok(render_node_meminfo())).into(),
            "cpulist" => {
                SimpleFile::new_regular(fs, || Ok(format!("{}\n", cpu_range_string()))).into()
            }
            "cpumap" => SimpleFile::new_regular(fs, || Ok(format!("{}\n", cpu_hex_mask()))).into(),
            // `distance` is unconditional in Linux `node_dev_attrs[]` (drivers/base/node.c:654):
            // `node_read_distance()` emits space-separated `node_distance(nid, i)` for each online
            // node. This single UMA node has only its self-distance, `LOCAL_DISTANCE == 10`
            // (include/linux/topology.h:46).
            "distance" => SimpleFile::new_regular(fs, || Ok("10\n".to_owned())).into(),
            _ => return Err(VfsError::NotFound),
        })
    }
}

/// `Node 0 {MemTotal,MemFree,MemUsed}` block that hwloc parses to learn per-node memory.
///
/// Matches Linux `drivers/base/node.c:node_read_meminfo()`: `MemTotal = totalram`,
/// `MemFree = freeram`, `MemUsed = totalram - freeram`, printed `"Node %d <field>: %8lu kB"`.
/// The free figure is the live allocator gauge (RAM minus the sum of every `UsageKind`
/// category), identical to what `/proc/meminfo` reports in `render_meminfo()`, so the two
/// views never contradict each other.
fn render_node_meminfo() -> String {
    let total = ax_runtime::hal::mem::total_ram_size();
    let usages = ax_alloc::global_allocator().usages();
    let used = super::allocator_used_bytes(&usages);
    let free = total.saturating_sub(used);

    // Derive the displayed values so the reported `MemUsed == MemTotal - MemFree`
    // identity is exact. Linux keeps it exact because `K()` scales page counts
    // linearly (`K(total) - K(free) == K(total - free)`); scaling bytes and
    // truncating each field independently would break it by up to 1 kB.
    let total_kb = total / 1024;
    let free_kb = free / 1024;
    let used_kb = total_kb - free_kb;
    format!(
        "Node 0 MemTotal:       {total_kb:>8} kB\nNode 0 MemFree:        {free_kb:>8} kB\nNode 0 \
         MemUsed:        {used_kb:>8} kB\n"
    )
}

/// Format a `nr_bits`-wide CPU bitmask in Linux sysfs form, `set(i)` reporting whether bit `i` is
/// set. Mirrors `bitmap_string()` (`lib/vsprintf.c`, the `%*pb` cpumask format used by
/// `cpumap_print_to_pagebuf`): comma-separated 32-bit hex groups, most-significant group first.
///
/// The width is the CPU count, not a fixed 64, so masks above 64 CPUs are not truncated. The
/// leading group is printed in `ceil(chunksz / 4)` hex digits where `chunksz = nr_bits % 32` (or 32
/// when `nr_bits` is a multiple of 32); every following group is a full zero-padded 8 hex digits.
/// A 65-CPU all-set mask thus renders `1,ffffffff,ffffffff` and a 128-CPU one four `ffffffff`
/// groups, matching Linux exactly.
fn format_cpu_mask(nr_bits: usize, set: impl Fn(usize) -> bool) -> String {
    let nr_bits = nr_bits.max(1);
    let mut chunksz = match nr_bits % 32 {
        0 => 32,
        rem => rem,
    };
    let mut out = String::new();
    // Walk 32-bit chunks most-significant first, aligned like Linux's `ALIGN(nr_bits, 32) - 32`.
    let mut base = nr_bits.next_multiple_of(32) - 32;
    loop {
        let val: u32 = (0..chunksz)
            .filter(|&b| set(base + b))
            .fold(0u32, |acc, b| acc | (1u32 << b));
        if !out.is_empty() {
            out.push(',');
        }
        let width = chunksz.div_ceil(4);
        out.push_str(&alloc::format!("{val:0width$x}"));
        chunksz = 32;
        if base == 0 {
            break;
        }
        base -= 32;
    }
    out
}

/// Hex CPU bitmask for all online CPUs, e.g. `f` for 4 CPUs.
fn cpu_hex_mask() -> String {
    let n = ax_runtime::hal::cpu_num();
    format_cpu_mask(n, |cpu| cpu < n)
}

/// Hex CPU bitmask with only `cpu` set (a no-SMT core owning exactly its own CPU).
fn cpu_bit_mask(cpu: usize) -> String {
    format_cpu_mask(ax_runtime::hal::cpu_num(), |i| i == cpu)
}

/// `/sys/devices/system/cpu/` — enough CPU topology for userspace to size pools.
struct SystemCpuDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for SystemCpuDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let mut names: Vec<Cow<'a, str>> = alloc::vec![
            Cow::Borrowed("online"),
            Cow::Borrowed("possible"),
            Cow::Borrowed("present"),
        ];
        names.extend((0..ax_runtime::hal::cpu_num()).map(|cpu| Cow::Owned(format!("cpu{cpu}"))));
        Box::new(names.into_iter())
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(match name {
            "online" | "possible" | "present" => {
                SimpleFile::new_regular(fs, || Ok(format!("{}\n", cpu_range_string()))).into()
            }
            _ => {
                let cpu = name
                    .strip_prefix("cpu")
                    .and_then(|s| s.parse::<usize>().ok())
                    .ok_or(VfsError::NotFound)?;
                if cpu >= ax_runtime::hal::cpu_num() {
                    return Err(VfsError::NotFound);
                }
                NodeOpsMux::Dir(SimpleDir::new_maker(
                    fs.clone(),
                    Arc::new(SystemCpuEntryDir { fs, cpu }),
                ))
            }
        })
    }
}

struct SystemCpuEntryDir {
    fs: Arc<SimpleFs>,
    cpu: usize,
}

impl SimpleDirOps for SystemCpuEntryDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let names = [
            Cow::Borrowed("online"),
            Cow::Borrowed("regs"),
            Cow::Borrowed("topology"),
        ]
        .into_iter();
        // `cache/` only exists when the architecture can enumerate real cache leaves; on
        // targets with no cache-geometry facility (e.g. riscv64, DT-only in Linux) it is
        // absent rather than filled with invented values.
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "loongarch64"))]
        let names =
            names.chain(has_cache_leaves(self.cpu).then_some(Cow::Borrowed("cache")));
        Box::new(names)
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match name {
            "online" => {
                let online = if self.cpu < ax_runtime::hal::cpu_num() {
                    "1\n"
                } else {
                    "0\n"
                };
                Ok(SimpleFile::new_regular(self.fs.clone(), move || Ok(online.to_owned())).into())
            }
            "regs" => Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                self.fs.clone(),
                Arc::new(CpuRegsDir {
                    fs: self.fs.clone(),
                    cpu: self.cpu,
                }),
            ))),
            // `cpuN/topology/` is what hwloc (used by pocl/lavapipe) probes to decide the Linux sysfs
            // backend is usable; without a cpumask topology file it aborts discovery and reports
            // total_memory=0 (so pocl advertises a 0-byte OpenCL device). `cache/` fills the cache
            // hierarchy hwloc reads next.
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "loongarch64"))]
            "cache" if has_cache_leaves(self.cpu) => {
                Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                    self.fs.clone(),
                    Arc::new(CpuCacheDir {
                        fs: self.fs.clone(),
                        cpu: self.cpu,
                    }),
                )))
            }
            "topology" => Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                self.fs.clone(),
                Arc::new(CpuTopologyDir {
                    fs: self.fs.clone(),
                    cpu: self.cpu,
                }),
            ))),
            _ => Err(VfsError::NotFound),
        }
    }
}

/// `/sys/devices/system/cpu/cpu<N>/topology/` - socket/core/thread map. `core_cpus` (a cpumask) is
/// the file hwloc requires to accept the Linux backend; each CPU is modelled as its own core (no
/// SMT) so hwloc's compute-unit count matches cpu_num.
struct CpuTopologyDir {
    fs: Arc<SimpleFs>,
    cpu: usize,
}

impl SimpleDirOps for CpuTopologyDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        // The `_list` and `thread_siblings` files mirror Linux `drivers/base/topology.c`, which
        // always emits both the hex-`_cpus` mask and its `_list` sibling for every mask attribute.
        let names = [
            "core_id",
            "physical_package_id",
            "core_cpus",
            "core_cpus_list",
            "thread_siblings",
            "thread_siblings_list",
            // `core_siblings{,_list}` are in `topology.c`'s `bin_attrs[]` with no `#ifdef`, so every
            // arch exposes them; they are backed by `core_cpumask` - the package/socket domain,
            // the same mask `package_cpus` renders.
            "core_siblings",
            "core_siblings_list",
            "package_cpus",
            "package_cpus_list",
        ]
        .into_iter();
        // `cluster_*` gate behind Linux's `TOPOLOGY_CLUSTER_SYSFS` (include/linux/topology.h:183),
        // set when the arch defines `topology_cluster_id`/`topology_cluster_cpumask`: x86_64
        // (arch/x86/include/asm/topology.h) plus aarch64/riscv64 via include/linux/arch_topology.h;
        // loongarch64 defines neither, so it omits them.
        #[cfg(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        ))]
        let names = names.chain(["cluster_id", "cluster_cpus", "cluster_cpus_list"]);
        // `die_*` gate behind `TOPOLOGY_DIE_SYSFS` (include/linux/topology.h:180), which needs
        // `topology_die_id`+`topology_die_cpumask` - defined only by x86_64 among the four targets
        // (arch/x86/include/asm/topology.h:147,201); aarch64/riscv64/loongarch64 define neither.
        #[cfg(target_arch = "x86_64")]
        let names = names.chain(["die_id", "die_cpus", "die_cpus_list"]);
        Box::new(names.map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let cpu = self.cpu;
        // A direct lookup must honour the same per-arch gating as `child_names`, otherwise a
        // name absent from the listing is still openable. `die_*` exists only under Linux's
        // TOPOLOGY_DIE_SYSFS (x86_64) and `cluster_*` only under TOPOLOGY_CLUSTER_SYSFS
        // (x86_64/aarch64/riscv64); elsewhere Linux returns ENOENT.
        #[cfg(not(target_arch = "x86_64"))]
        if matches!(name, "die_id" | "die_cpus" | "die_cpus_list") {
            return Err(VfsError::NotFound);
        }
        #[cfg(not(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )))]
        if matches!(name, "cluster_id" | "cluster_cpus" | "cluster_cpus_list") {
            return Err(VfsError::NotFound);
        }
        match name {
            "core_id" => {
                Ok(
                    SimpleFile::new_regular(self.fs.clone(), move || Ok(alloc::format!("{cpu}\n")))
                        .into(),
                )
            }
            // `die_id` only reachable on x86_64 (see `child_names` gating); the same "0\n" single
            // socket/die value as `physical_package_id`. `cluster_id` is the CPU's cluster index,
            // 0 in the single no-sub-package-cluster model StarryOS uses.
            "physical_package_id" | "die_id" | "cluster_id" => {
                Ok(SimpleFile::new_regular(self.fs.clone(), || Ok("0\n".to_owned())).into())
            }
            // no-SMT: this core / thread owns exactly its own CPU. `cluster_cpus` is the CPU's own
            // cluster mask; with no sub-package cluster modelled it is the CPU itself, exactly what
            // Linux's default `clear_cpu_topology()` leaves in `cluster_sibling` (arch_topology.c:
            // 792-793) for a CPU whose firmware declares no cluster.
            "core_cpus" | "thread_siblings" | "cluster_cpus" => {
                Ok(SimpleFile::new_regular(self.fs.clone(), move || {
                    Ok(alloc::format!("{}\n", cpu_bit_mask(cpu)))
                })
                .into())
            }
            // Single self-CPU list (no SMT / no sub-package cluster): just this CPU's number.
            "core_cpus_list" | "thread_siblings_list" | "cluster_cpus_list" => {
                Ok(SimpleFile::new_regular(self.fs.clone(), move || Ok(format!("{cpu}\n"))).into())
            }
            // `core_siblings` is Linux's `core_cpumask` = the package/socket domain, identical to
            // `package_cpus`; `die_cpus` is x86-only and (single die) also spans every online CPU.
            "package_cpus" | "die_cpus" | "core_siblings" => {
                Ok(SimpleFile::new_regular(self.fs.clone(), || {
                    Ok(alloc::format!("{}\n", cpu_hex_mask()))
                })
                .into())
            }
            // System-wide list (all online CPUs) with the single terminating newline.
            "package_cpus_list" | "die_cpus_list" | "core_siblings_list" => {
                Ok(SimpleFile::new_regular(self.fs.clone(), || {
                    Ok(format!("{}\n", cpu_range_string()))
                })
                .into())
            }
            _ => Err(VfsError::NotFound),
        }
    }
}

/// `/sys/devices/system/cpu/cpu<N>/regs/` — `identification/midr_el1` for perf.
struct CpuRegsDir {
    fs: Arc<SimpleFs>,
    cpu: usize,
}

impl SimpleDirOps for CpuRegsDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(["identification"].into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match name {
            "identification" => Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                self.fs.clone(),
                Arc::new(CpuIdRegsDir {
                    fs: self.fs.clone(),
                    cpu: self.cpu,
                }),
            ))),
            _ => Err(VfsError::NotFound),
        }
    }
}

/// `/sys/devices/system/cpu/cpu<N>/regs/identification/` — `midr_el1` for perf.
struct CpuIdRegsDir {
    fs: Arc<SimpleFs>,
    cpu: usize,
}

impl SimpleDirOps for CpuIdRegsDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(["midr_el1"].into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match name {
            "midr_el1" => {
                let cpu = self.cpu;
                Ok(SimpleFile::new_regular(self.fs.clone(), move || {
                    let midr = crate::perf::cpu_midr(cpu);
                    Ok(alloc::format!("{midr:016x}\n"))
                })
                .into())
            }
            _ => Err(VfsError::NotFound),
        }
    }
}

/// The online-CPU range as a bare sysfs `cpulist` string (`0` or `0-N`), *without*
/// a trailing newline. Callers append the single terminating `\n` a sysfs attribute
/// requires, so the newline is owned at exactly one place per attribute and cannot be
/// doubled up (Linux emits one `\n` per single-line node/cpu list attribute).
fn cpu_range_string() -> String {
    cpu_range(ax_runtime::hal::cpu_num())
}

/// The `0`/`0-N` range for `n` online CPUs, split out from [`cpu_range_string`] so the
/// newline-free byte format is unit-testable without the HAL (the double-`\n` regression
/// only shows in the rendered attribute bytes).
fn cpu_range(n: usize) -> String {
    if n <= 1 {
        "0".to_owned()
    } else {
        format!("0-{}", n - 1)
    }
}

/// `/sys/devices/virtual/` — one subdirectory per subsystem hosting
/// "virtual" (non-bus) devices.
struct VirtualDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for VirtualDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(["drm", "graphics", "input"].into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(NodeOpsMux::Dir(match name {
            "drm" => SimpleDir::new_maker(
                fs.clone(),
                Arc::new(DeviceContainer::new(
                    fs,
                    "drm",
                    &[("card0", (DRM_MAJOR, 0), "dri/card0")],
                )),
            ),
            "graphics" => SimpleDir::new_maker(
                fs.clone(),
                Arc::new(DeviceContainer::new(
                    fs,
                    "graphics",
                    &[("fb0", (FB_MAJOR, 0), "fb0")],
                )),
            ),
            "input" => SimpleDir::new_maker(fs.clone(), Arc::new(InputDevicesDir { fs })),
            _ => return Err(VfsError::NotFound),
        }))
    }
}

/// Per-subsystem container under `/sys/devices/virtual/<subsystem>/`
/// with a static list of children.
struct DeviceContainer {
    fs: Arc<SimpleFs>,
    subsystem: &'static str,
    /// (name, (major, minor), devname-in-/dev)
    entries: Vec<(&'static str, (u32, u32), &'static str)>,
}

impl DeviceContainer {
    fn new(
        fs: Arc<SimpleFs>,
        subsystem: &'static str,
        entries: &[(&'static str, (u32, u32), &'static str)],
    ) -> Self {
        Self {
            fs,
            subsystem,
            entries: entries.to_vec(),
        }
    }
}

impl SimpleDirOps for DeviceContainer {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(self.entries.iter().map(|(n, ..)| Cow::Borrowed(*n)))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let (_, dev, devname) = *self
            .entries
            .iter()
            .find(|(n, ..)| *n == name)
            .ok_or(VfsError::NotFound)?;
        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            self.fs.clone(),
            Arc::new(DeviceAttributesDir {
                fs: self.fs.clone(),
                subsystem: self.subsystem,
                name: name.to_owned(),
                dev,
                devname: devname.to_owned(),
                parent_kind: ParentKind::ClassRoot,
            }),
        )))
    }
}

/// `/sys/devices/virtual/input/` — one `input<N>` parent per evdev
/// device, with an `event<N>` child underneath.  Matches Linux's
/// nesting so `udev_device_get_parent()` on an event node returns the
/// `inputN` container.
struct InputDevicesDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for InputDevicesDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let names: Vec<_> = (0..input_device_count())
            .map(|i| Cow::Owned(format!("input{i}")))
            .collect();
        Box::new(names.into_iter())
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let n = name
            .strip_prefix("input")
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or(VfsError::NotFound)?;
        if n >= input_device_count() {
            return Err(VfsError::NotFound);
        }
        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            self.fs.clone(),
            Arc::new(InputParentDir {
                fs: self.fs.clone(),
                index: n,
            }),
        )))
    }
}

/// `/sys/devices/virtual/input/input<N>/` — the parent container for an
/// evdev device.  Holds its own `uevent` + `subsystem` so `udevadm info`
/// can walk through it, plus the `event<N>` child dir.
struct InputParentDir {
    fs: Arc<SimpleFs>,
    index: u32,
}

impl SimpleDirOps for InputParentDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let event = format!("event{}", self.index);
        Box::new(
            [
                Cow::Borrowed("uevent"),
                Cow::Borrowed("name"),
                Cow::Borrowed("subsystem"),
                Cow::Owned(event),
            ]
            .into_iter(),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        let n = self.index;
        Ok(match name {
            "uevent" => SimpleFile::new_regular(fs, move || {
                let mut body = format!("PRODUCT=0/0/0/0\nNAME=\"starry-input{n}\"\n");
                for tag in EVDEV_TAGS {
                    body.push_str(tag);
                    body.push_str("=1\n");
                }
                Ok(body)
            })
            .into(),
            "name" => {
                let body = format!("starry-input{n}\n");
                SimpleFile::new_regular(fs, move || Ok(body.clone())).into()
            }
            "subsystem" => SimpleFile::new(fs, NodeType::Symlink, || {
                Ok("../../../../class/input".to_owned())
            })
            .into(),
            _ if name == format!("event{n}") => NodeOpsMux::Dir(SimpleDir::new_maker(
                self.fs.clone(),
                Arc::new(DeviceAttributesDir {
                    fs: self.fs.clone(),
                    subsystem: "input",
                    name: name.to_owned(),
                    dev: (INPUT_MAJOR, EVDEV_MINOR_BASE + n),
                    devname: format!("input/event{n}"),
                    parent_kind: ParentKind::InputInputN,
                }),
            )),
            _ => return Err(VfsError::NotFound),
        })
    }
}

/// Where does this device's `device` symlink / parent-chain point?
#[derive(Clone, Copy, Debug)]
enum ParentKind {
    ClassRoot,
    InputInputN,
}

/// The attribute directory for a single device.
struct DeviceAttributesDir {
    fs: Arc<SimpleFs>,
    subsystem: &'static str,
    name: String,
    dev: (u32, u32),
    devname: String,
    parent_kind: ParentKind,
}

impl SimpleDirOps for DeviceAttributesDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            ["uevent", "dev", "name", "subsystem", "device"]
                .into_iter()
                .map(Cow::Borrowed),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(match name {
            "uevent" => {
                let (major, minor) = self.dev;
                let devname = self.devname.clone();
                let subsystem = self.subsystem;
                SimpleFile::new_regular(fs, move || {
                    let mut buf = format!(
                        "MAJOR={major}\nMINOR={minor}\nDEVNAME={devname}\nSUBSYSTEM={subsystem}\n"
                    );
                    if subsystem == "input" && devname.starts_with("input/event") {
                        for tag in EVDEV_TAGS {
                            buf.push_str(tag);
                            buf.push_str("=1\n");
                        }
                    }
                    Ok(buf)
                })
                .into()
            }
            "dev" => {
                let (major, minor) = self.dev;
                SimpleFile::new_regular(fs, move || Ok(format!("{major}:{minor}\n"))).into()
            }
            "name" => {
                let body = format!("{}\n", self.name);
                SimpleFile::new_regular(fs, move || Ok(body.clone())).into()
            }
            "subsystem" => {
                // /sys/class/<subsystem>, relative from the real devpath.
                // ClassRoot   depth: devices/virtual/<subsystem>/<name>          → 3 ups.
                // InputInputN depth: devices/virtual/input/inputN/eventN         → 4 ups.
                let ups = match self.parent_kind {
                    ParentKind::ClassRoot => "../../../..",
                    ParentKind::InputInputN => "../../../../..",
                };
                let target = format!("{}/class/{}", ups, self.subsystem);
                SimpleFile::new(fs, NodeType::Symlink, move || Ok(target.clone())).into()
            }
            "device" => {
                // Parent-device symlink. For DRM/graphics cards we point at
                // /sys/devices/platform/virtio-gpu0 so Mesa's loader can
                // read PCI vendor/device files; without those, EGL init
                // fails with "failed to retrieve device information".
                let target = match (self.parent_kind, self.subsystem) {
                    (ParentKind::ClassRoot, "drm") | (ParentKind::ClassRoot, "graphics") => {
                        "../../../../devices/platform/virtio-gpu0".to_owned()
                    }
                    (ParentKind::ClassRoot, _) => "..".to_owned(),
                    (ParentKind::InputInputN, _) => "..".to_owned(),
                };
                SimpleFile::new(fs, NodeType::Symlink, move || Ok(target.clone())).into()
            }
            _ => return Err(VfsError::NotFound),
        })
    }
}

// ========================================================================
// /sys/devices/platform — parent-bus stubs.
// ========================================================================

struct PlatformBusDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for PlatformBusDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            ["virtio-gpu0", "virtio-input"]
                .into_iter()
                .map(Cow::Borrowed),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let driver = match name {
            "virtio-gpu0" => "virtio-gpu",
            "virtio-input" => "virtio-input",
            _ => return Err(VfsError::NotFound),
        };
        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            self.fs.clone(),
            Arc::new(PlatformDeviceDir {
                fs: self.fs.clone(),
                driver,
            }),
        )))
    }
}

struct PlatformDeviceDir {
    fs: Arc<SimpleFs>,
    driver: &'static str,
}

impl SimpleDirOps for PlatformDeviceDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        // virtio-gpu0 also exposes PCI-style identifiers so Mesa's DRI
        // loader can match the device to a driver.
        let mut names: Vec<&'static str> = alloc::vec!["uevent", "subsystem"];
        if self.driver == "virtio-gpu" {
            names.extend_from_slice(&[
                "vendor",
                "device",
                "subsystem_vendor",
                "subsystem_device",
                "revision",
                "class",
            ]);
        }
        Box::new(names.into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(match name {
            "uevent" => {
                let driver = self.driver.to_owned();
                SimpleFile::new_regular(fs, move || {
                    Ok(format!("DRIVER={driver}\nSUBSYSTEM=platform\n"))
                })
                .into()
            }
            "subsystem" => SimpleFile::new(fs, NodeType::Symlink, || {
                Ok("../../../bus/platform".to_owned())
            })
            .into(),
            // virtio-gpu PCI IDs per upstream. Format matches what the
            // PCI subsystem emits: "0xNNNN\n".
            "vendor" if self.driver == "virtio-gpu" => {
                SimpleFile::new_regular(fs, || Ok("0x1af4\n".to_owned())).into()
            }
            "device" if self.driver == "virtio-gpu" => {
                SimpleFile::new_regular(fs, || Ok("0x1050\n".to_owned())).into()
            }
            "subsystem_vendor" if self.driver == "virtio-gpu" => {
                SimpleFile::new_regular(fs, || Ok("0x1af4\n".to_owned())).into()
            }
            "subsystem_device" if self.driver == "virtio-gpu" => {
                SimpleFile::new_regular(fs, || Ok("0x1100\n".to_owned())).into()
            }
            "revision" if self.driver == "virtio-gpu" => {
                SimpleFile::new_regular(fs, || Ok("0x01\n".to_owned())).into()
            }
            "class" if self.driver == "virtio-gpu" => {
                // PCI class 0x030000 = display controller / VGA.
                SimpleFile::new_regular(fs, || Ok("0x030000\n".to_owned())).into()
            }
            _ => return Err(VfsError::NotFound),
        })
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    // Byte-exact regression for the `node0/cpulist` double-newline ABI fix (#1573).
    // `cpu_range_string()` embedded its own `\n`, so the attribute renderer's
    // `format!("{}\n", cpu_range_string())` emitted `0\n\n` (`0-N\n\n` for SMP) -
    // an illegal extra blank line for a single-line sysfs attribute a byte-exact
    // user-space reader (`cat`, hwloc) parses. The range helper now returns the
    // bare `0`/`0-N`, and every attribute owns the single terminating `\n`.
    #[test]
    fn cpu_range_has_no_embedded_newline() {
        for n in [0usize, 1, 2, 4, 64, 65, 128] {
            let range = cpu_range(n);
            // Root-cause invariant: the range carries no newline of its own, so no
            // caller can double it up. On the buggy `"0\n"`/`"0-N\n"` helper this
            // fails immediately.
            assert!(
                !range.contains('\n'),
                "cpu_range({n}) must not embed a newline, got {range:?}"
            );
        }
    }

    // The rendered sysfs attribute bytes: exactly one trailing `\n`, never `\n\n`.
    // Mirrors how `node0/cpulist`, `cpu/{online,possible,present}`, PMU `cpus`, and
    // the `*_list` topology attributes render (`format!("{}\n", cpu_range_string())`).
    #[test]
    fn cpulist_attribute_has_single_trailing_newline() {
        // Uniprocessor and SMP both terminate with a single `\n`.
        let uni = format!("{}\n", cpu_range(1));
        assert_eq!(uni, "0\n", "smp1 cpulist must be exactly `0\\n`");
        let smp = format!("{}\n", cpu_range(4));
        assert_eq!(smp, "0-3\n", "smp4 cpulist must be exactly `0-3\\n`");

        for rendered in [&uni, &smp] {
            assert!(
                rendered.ends_with('\n') && !rendered.ends_with("\n\n"),
                "cpulist attribute must end with a single newline, got {rendered:?}"
            );
            assert_eq!(
                rendered.matches('\n').count(),
                1,
                "cpulist attribute must contain exactly one newline, got {rendered:?}"
            );
        }
    }
}

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
use core::sync::atomic::{AtomicBool, Ordering};

use ax_ipi::legacy::run_on_cpu;
use ax_lazyinit::OnceLock;
use ax_runtime::hal::{cpu_num, percpu::this_cpu_id, time::monotonic_time_nanos};
use axfs_ng_vfs::{Filesystem, NodeType, VfsError, VfsResult};

use crate::{
    pseudofs::{DirMaker, DirMapping, NodeOpsMux, SimpleDir, SimpleDirOps, SimpleFile, SimpleFs},
    sync::IrqMutex,
};

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
        let tracing = PERF_EVENT_SOURCES.iter().map(|(n, _)| Cow::Borrowed(*n));
        // The ARM PMUv3 CPU PMU is a hardware (counting/sampling) event source,
        // distinct from the tracing sources above. It is always listed on
        // aarch64 — the device is a static description of the architectural PMU,
        // so it is advertised regardless of `ax_hal::pmu::info()` (which a
        // `perf_event_open` against the type still consults and may reject).
        #[cfg(target_arch = "aarch64")]
        let pmu = core::iter::once(Cow::Borrowed(ARMV8_PMUV3_DEVICE));
        #[cfg(not(target_arch = "aarch64"))]
        let pmu = core::iter::empty();
        Box::new(tracing.chain(pmu))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        #[cfg(target_arch = "aarch64")]
        if name == ARMV8_PMUV3_DEVICE {
            return Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                fs.clone(),
                Arc::new(HwPmuDeviceDir { fs }),
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
                let body = format!("{}\n", crate::perf::hw::ARMV8_PMUV3_PERF_TYPE);
                Ok(SimpleFile::new_regular(fs, move || Ok(body.clone())).into())
            }
            // CPUs this PMU covers; reuse the shared online-CPU range ("0\n"
            // under smp1). Matches Linux's `<pmu>/cpus`.
            "cpus" => {
                Ok(SimpleFile::new_regular(fs, || Ok(format!("{}\n", cpu_range_string()))).into())
            }
            "format" => Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                fs.clone(),
                Arc::new(HwPmuFormatDir { fs }),
            ))),
            "events" => Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                fs.clone(),
                Arc::new(HwPmuEventsDir { fs }),
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
}

#[cfg(target_arch = "aarch64")]
impl SimpleDirOps for HwPmuEventsDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(ARMV8_PMUV3_EVENTS.iter().map(|(n, _)| Cow::Borrowed(*n)))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        let event = ARMV8_PMUV3_EVENTS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, e)| *e)
            .ok_or(VfsError::NotFound)?;
        let body = format!("event={event:#04x}\n");
        Ok(SimpleFile::new_regular(fs, move || Ok(body.clone())).into())
    }
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
    let used = usages.get(ax_alloc::UsageKind::RustHeap)
        + usages.get(ax_alloc::UsageKind::VirtMem)
        + usages.get(ax_alloc::UsageKind::PageCache)
        + usages.get(ax_alloc::UsageKind::PageTable)
        + usages.get(ax_alloc::UsageKind::Dma)
        + usages.get(ax_alloc::UsageKind::Global);
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
        let mut names: Vec<Cow<'a, str>> = alloc::vec![
            Cow::Borrowed("online"),
            Cow::Borrowed("regs"),
            Cow::Borrowed("topology"),
        ];
        // `cache/` only exists when the architecture can enumerate real cache leaves; on
        // targets with no cache-geometry facility (e.g. riscv64, DT-only in Linux) it is
        // absent rather than filled with invented values.
        if !cpu_cache_leaves(self.cpu).is_empty() {
            names.push(Cow::Borrowed("cache"));
        }
        Box::new(names.into_iter())
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
                }),
            ))),
            // `cpuN/topology/` is what hwloc (used by pocl/lavapipe) probes to decide the Linux sysfs
            // backend is usable; without a cpumask topology file it aborts discovery and reports
            // total_memory=0 (so pocl advertises a 0-byte OpenCL device). `cache/` fills the cache
            // hierarchy hwloc reads next.
            "cache" if !cpu_cache_leaves(self.cpu).is_empty() => {
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

/// Cache type as reported by the arch's cache-detection facility. Mirrors Linux
/// `enum cache_type` (`include/linux/cacheinfo.h`) and its `type_show()` strings.
///
/// On riscv64 no arch cache-read path is compiled (there is no cache-geometry
/// register), so these variants are never constructed there - allow it rather
/// than emit an arch-specific dead-code warning.
#[cfg_attr(
    not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "loongarch64"
    )),
    allow(dead_code)
)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum CacheType {
    Data,
    Instruction,
    Unified,
}

impl CacheType {
    fn as_str(self) -> &'static str {
        match self {
            CacheType::Data => "Data",
            CacheType::Instruction => "Instruction",
            CacheType::Unified => "Unified",
        }
    }
}

/// Which CPUs share a given cache leaf, mirroring how Linux builds
/// `cacheinfo.shared_cpu_map`.
///
/// Linux fills `shared_cpu_map` on two distinct paths:
///
/// - **aarch64 / loongarch64** enumerate from architecture registers with no
///   thread-sharing count, i.e. the `use_arch_info` path in
///   `drivers/base/cacheinfo.c`. There `cache_leaves_are_shared()`
///   (cacheinfo.c:47-49) reduces to `this_leaf->level != 1 && sib_leaf->level
///   != 1`: L1 is private per CPU, every higher level is system-wide shared.
///
/// - **x86** overrides that with `__cache_cpumap_setup()`
///   (arch/x86/kernel/cpu/cacheinfo.c:546-580), which uses the per-leaf
///   `num_threads_sharing` count from `CPUID.4` to build a bounded APIC domain
///   `apicid >> order_base_2(num_threads_sharing)`. Under the single-socket,
///   no-SMT, no-sub-package-cluster topology this sysfs tree models (every
///   `cpuN/topology/` reports `physical_package_id 0`, `core_id == cpu`, and a
///   `package_cpus` mask of all online CPUs), a consistent leaf is per-core when
///   `num_threads_sharing == 1` (private) or package-wide when shared by every
///   online CPU (the same set `package_cpus` / `core_siblings` render). A leaf
///   whose count falls strictly between - shared by a subset larger than one -
///   would need the exact APIC domain this flat tree cannot reconstruct and also
///   contradicts the no-SMT/single-package geometry the rest of the tree
///   presents; it is recorded as [`Unknown`] so the ambiguous `shared_cpu_map` /
///   `shared_cpu_list` are omitted rather than fabricated (neither owner-only nor
///   whole-package). The homogeneous `-smp 4` QEMU models never produce it.
///
/// `SharingScope` records the per-leaf decision so `shared_cpu_map` /
/// `shared_cpu_list` can be rebuilt for any logical CPU.
///
/// [`Unknown`]: SharingScope::Unknown
#[derive(Clone, Copy, PartialEq, Eq)]
enum SharingScope {
    /// The owning CPU only (Linux's L1-private rule on the arch-info path, and
    /// x86 leaves whose `num_threads_sharing == 1`). With no SMT modelled the
    /// owner has no thread siblings, so this is exactly `{cpu}`.
    Private,
    /// Every online CPU: the package domain. This is Linux's "system-wide
    /// shared" rule for L2+ caches on the `use_arch_info` path (aarch64 /
    /// loongarch64, which carry no thread-sharing count), and equally the
    /// package-wide domain any x86 leaf shared beyond its owner collapses to
    /// under this single-socket topology (see the type doc).
    SystemWide,
    /// A cache shared beyond its owner but by a strict subset of the online CPUs
    /// (an x86 leaf with `1 < num_threads_sharing < online`). The exact members
    /// need the per-CPU APIC-id grouping `__cache_cpumap_setup()` performs
    /// (`apicid >> order_base_2(num_threads_sharing)`), which this flat-topology
    /// tree cannot reconstruct. Such a leaf also contradicts the flat topology
    /// the rest of the tree presents (no SMT, single package), so rather than
    /// widen the set to every CPU (over-reporting a partial cache as package-wide)
    /// or shrink it to the owner (under-reporting), `shared_cpu_map` /
    /// `shared_cpu_list` are *omitted*, exactly as Linux omits any cache attribute
    /// it cannot back with a real value. Never produced by the register-only path
    /// or by the homogeneous `-smp 4` QEMU models (where a leaf is shared by 1 or
    /// by all CPUs), so it is guarded by the `arch_info_scope` unit test.
    Unknown,
}

/// One cache leaf as enumerated from firmware/architecture registers, mirroring Linux's
/// `struct cacheinfo`. Geometry fields are `Option` so that any attribute the hardware does
/// not report is *omitted* from sysfs rather than fabricated - exactly like Linux's
/// `cache_default_attrs_is_visible()`, which hides an attribute whose backing value is 0.
#[derive(Clone, Copy)]
struct CacheLeaf {
    level: u32,
    ctype: CacheType,
    /// Total cache size in bytes.
    size: Option<u32>,
    /// `coherency_line_size` in bytes.
    line_size: Option<u32>,
    /// `number_of_sets`.
    sets: Option<u32>,
    /// `ways_of_associativity`.
    ways: Option<u32>,
    /// `physical_line_partition` (lines per tag).
    partition: Option<u32>,
    /// Which CPUs share this leaf, per Linux `cache_leaves_are_shared()` on the
    /// arch-info path (see [`SharingScope`]).
    sharing: SharingScope,
}

impl CacheLeaf {
    /// Assign the sharing scope for this leaf.
    ///
    /// `threads_sharing` is `None` on the register-only `use_arch_info` path
    /// (aarch64 / loongarch64), where Linux's `cache_leaves_are_shared()`
    /// (cacheinfo.c:47-49) gives L1 private and every higher level system-wide.
    ///
    /// x86 passes `Some(num_threads_sharing)` from `CPUID.4`, mapped against the
    /// online CPU count (the package size this flat tree models) by
    /// [`arch_info_scope`]:
    /// - `num_threads_sharing == 1` (or L1) -> the owning CPU only, matching the
    ///   early return in `__cache_cpumap_setup()` (cacheinfo.c:564).
    /// - shared by every online CPU -> the whole package (every online CPU, the
    ///   mask `package_cpus` / `core_siblings` also render), so [`SystemWide`].
    /// - shared by a strict subset (`1 < n < online`) -> [`Unknown`], whose exact
    ///   members `__cache_cpumap_setup()` derives from per-CPU APIC ids this tree
    ///   does not track; the attribute is omitted rather than fabricated (neither
    ///   owner-only under-reporting nor whole-package over-reporting).
    ///
    /// [`SystemWide`]: SharingScope::SystemWide
    /// [`Unknown`]: SharingScope::Unknown
    fn with_arch_info_sharing(mut self, threads_sharing: Option<u32>) -> Self {
        self.sharing = arch_info_scope(self.level, threads_sharing, cpu_num() as u32);
        self
    }
}

/// Sharing scope for a leaf sampled on the register-only `use_arch_info` path,
/// from its `level` and, on x86, `CPUID.4` `num_threads_sharing`. Split out from
/// [`CacheLeaf::with_arch_info_sharing`] so it can be unit-tested without a live
/// CPU: the bug it guards (a cache shared beyond its owner reported owner-only)
/// only manifests on a topology with intermediate cache sharing, which the
/// homogeneous `-smp 4` QEMU used by the on-target test never produces.
fn arch_info_scope(level: u32, threads_sharing: Option<u32>, online_cpus: u32) -> SharingScope {
    match (level, threads_sharing) {
        // L1, or an x86 leaf CPUID.4 reports as used by a single thread: private.
        (1, _) | (_, Some(1)) => SharingScope::Private,
        // x86 leaf shared by (at least) every online CPU: provably the whole
        // package. `>=` covers a count rounded up past the online set.
        (_, Some(n)) if n >= online_cpus => SharingScope::SystemWide,
        // x86 leaf shared by a strict subset (`1 < n < online`): the exact members
        // need APIC grouping this flat model lacks, so omit rather than fabricate
        // an all-CPU set (see [`SharingScope::Unknown`]).
        (_, Some(_)) => SharingScope::Unknown,
        // Register-only path (aarch64 / loongarch64, no count): L1 private (handled
        // above), every higher level system-wide - Linux's `cache_leaves_are_shared()`.
        (_, None) => SharingScope::SystemWide,
    }
}

/// Enumerate the **executing PE's** real cache hierarchy from architecture registers.
///
/// The architecture cache registers can only describe the core currently
/// running the instruction (CPUID / CCSIDR / CPUCFG are all per-PE), so this
/// must run on a real CPU, not a later `cpuN/cache` read that may land on any
/// PE. It is called once at boot from [`init_cpu_cache`] on the primary CPU and
/// the result is served for every logical CPU (see [`cpu_cache_leaves`]).
///
/// - x86_64: `CPUID` leaf 4 (deterministic cache parameters), decoded per
///   `arch/x86/kernel/cpu/cacheinfo.c` (`cpuid4_info_fill_done`).
/// - aarch64: `CLIDR_EL1` for present levels/types (`arch/arm64/kernel/cacheinfo.c`
///   `get_cache_type`) and `CCSIDR_EL1` (selected via `CSSELR_EL1`) for the geometry, the
///   ARM-architected cache-size register.
/// - loongarch64: `CPUCFG` leaves 16/17.. (`arch/loongarch/mm/cache.c` `cpu_cache_init`).
/// - riscv64: RISC-V defines no cache-geometry registers; Linux relies solely on the device
///   tree (`arch/riscv/kernel/cacheinfo.c` `init_of_cache_level`). With no DT cacheinfo
///   parser here, no leaf can be produced without fabricating, so the list is empty and the
///   `cache/` directory is not exposed - the same "unavailable => absent" outcome Linux
///   gives when firmware carries no cacheinfo.
fn read_local_cache_leaves() -> Vec<CacheLeaf> {
    #[cfg(target_arch = "x86_64")]
    {
        read_cache_leaves_x86()
    }
    #[cfg(target_arch = "aarch64")]
    {
        read_cache_leaves_aarch64()
    }
    #[cfg(target_arch = "loongarch64")]
    {
        read_cache_leaves_loongarch64()
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "loongarch64"
    )))]
    {
        Vec::new()
    }
}

/// Per-CPU cache leaves, each sampled *on its own logical CPU* during boot.
///
/// This mirrors Linux's `per_cpu(ci_cpu_cacheinfo, cpu)`
/// (`drivers/base/cacheinfo.c:27`), which detects each CPU's cache in the
/// `cacheinfo_cpu_online` hotplug callback so a heterogeneous (big.LITTLE)
/// machine reports the true geometry of every cluster. [`init_cpu_cache`] drives
/// the equivalent: it runs [`read_local_cache_leaves`] on each online CPU - the
/// primary directly, every secondary through an IPI - so `slot[cpu]` holds the
/// cache registers of `cpu` itself, not a copy of the primary's.
///
/// `OnceLock` guards the one-time table allocation. Each slot is an IRQ-safe
/// [`IrqMutex`] because it is written from the IPI handler on the sampled CPU and
/// read later from `cpuN/cache` sysfs reads; the `Vec<CacheLeaf>` is heap-backed
/// (the allocator is up by the time sysfs mounts).
static PER_CPU_CACHE_LEAVES: OnceLock<Vec<IrqMutex<Vec<CacheLeaf>>>> = OnceLock::new();

/// Upper bound on the wait for one secondary CPU to answer the sampling IPI. A
/// CPU that never responds (e.g. a hart parked beyond `CPU_CAPACITY`) leaves its
/// slot empty; [`cpu_cache_leaves`] then reports no leaves for that CPU (so its
/// `cache/` tree is absent) rather than ever serving another CPU's registers.
const CACHE_SAMPLE_WAIT_NS: u64 = 1_000_000_000;

/// Detect every online CPU's cache leaves, each on that CPU.
///
/// The cache registers (`CPUID` / `CCSIDR` / `CPUCFG`) only describe the
/// executing core, so each CPU must read its own: the primary reads directly and
/// every secondary is driven through `ax_ipi::legacy::run_on_cpu` to read on that
/// PE and store into its own slot - the same cross-CPU mechanism `stop_machine` uses.
/// Mirrors Linux running `detect_cache_attributes(cpu)` on `cpu` at its online
/// callback. Called from `mount_all` on the primary once all secondaries are up,
/// before sysfs is mounted.
pub fn init_cpu_cache() {
    let total = cpu_num();
    let table =
        PER_CPU_CACHE_LEAVES.call_once(|| (0..total).map(|_| IrqMutex::new(Vec::new())).collect());

    let current = this_cpu_id();
    if let Some(slot) = table.get(current) {
        *slot.lock() = read_local_cache_leaves();
    }

    for cpu in 0..total {
        if cpu == current {
            continue;
        }
        let done = Arc::new(AtomicBool::new(false));
        let remote_done = done.clone();
        // A CPU the IPI layer refuses (e.g. beyond the online set) leaves its slot
        // empty, exactly like the timeout below; skip its wait rather than block.
        if run_on_cpu(cpu, move || {
            let leaves = read_local_cache_leaves();
            if let Some(slot) = PER_CPU_CACHE_LEAVES.get().and_then(|table| table.get(cpu)) {
                *slot.lock() = leaves;
            }
            remote_done.store(true, Ordering::Release);
        })
        .is_err()
        {
            continue;
        }
        // Bounded wait: an online secondary answers promptly; a CPU that never
        // runs the callback times out and keeps its empty slot, which
        // `cpu_cache_leaves` then reports as "no leaves" rather than another
        // CPU's data.
        let start = monotonic_time_nanos();
        while !done.load(Ordering::Acquire) {
            core::hint::spin_loop();
            if monotonic_time_nanos().saturating_sub(start) > CACHE_SAMPLE_WAIT_NS {
                break;
            }
        }
    }
}

/// The cache leaves reported for logical CPU `cpu`, sampled on that CPU.
///
/// Returns the set [`init_cpu_cache`] read on `cpu` itself (see
/// [`PER_CPU_CACHE_LEAVES`]), so heterogeneous cores report their own geometry
/// rather than the primary's.
///
/// Invariant: `/sys/.../cpuN/cache` must reflect `cpuN`'s own registers no
/// matter which PE services the read. The architecture cache registers describe
/// only the executing core, so a live [`read_local_cache_leaves`] is valid for
/// `cpu` *only* when it runs on `cpu`. Once the per-CPU table is built, that
/// pinned slot is the single source of truth; a slot left empty by a timed-out
/// sampling IPI yields an empty result (so `cpuN/cache` is absent) rather than
/// being backfilled from the reader's PE, which would return another CPU's data.
/// A live read is used only for the pre-`init_cpu_cache` boot race, and only
/// when `cpu == this_cpu_id()` so it still describes `cpu`.
fn cpu_cache_leaves(cpu: usize) -> Vec<CacheLeaf> {
    match PER_CPU_CACHE_LEAVES.get() {
        Some(table) => table
            .get(cpu)
            .map(|slot| slot.lock().clone())
            .unwrap_or_default(),
        None if cpu == this_cpu_id() => read_local_cache_leaves(),
        None => Vec::new(),
    }
}

/// x86_64 cache enumeration, mirroring `arch/x86/kernel/cpu/cacheinfo.c`:
/// prefer `CPUID` leaf 4 (deterministic cache parameters); when it reports no
/// caches, fall back to the legacy `CPUID` leaf 0x2 descriptor table
/// (`intel_cacheinfo_0x2`), exactly as Linux does when leaf 4 is unavailable.
#[cfg(target_arch = "x86_64")]
fn read_cache_leaves_x86() -> Vec<CacheLeaf> {
    let leaves = read_cache_leaves_x86_leaf4();
    if !leaves.is_empty() {
        return leaves;
    }
    read_cache_leaves_x86_leaf2()
}

/// `CPUID` leaf 4: iterate subleaves until a NULL cache type, decoding EAX/EBX/ECX per
/// Intel SDM / `arch/x86/kernel/cpu/cacheinfo.c`. `size = (sets+1)*(line+1)*(part+1)*(ways+1)`.
#[cfg(target_arch = "x86_64")]
fn read_cache_leaves_x86_leaf4() -> Vec<CacheLeaf> {
    let mut leaves = Vec::new();
    for subleaf in 0..u32::MAX {
        // `__cpuid_count` is safe on bare-metal x86_64 targets (SSE/CPUID always present).
        #[allow(unused_unsafe)]
        let r = unsafe { core::arch::x86_64::__cpuid_count(4, subleaf) };
        let cache_type = r.eax & 0x1f;
        let ctype = match cache_type {
            1 => CacheType::Data,
            2 => CacheType::Instruction,
            3 => CacheType::Unified,
            _ => break, // 0 = NULL: no more caches.
        };
        let level = (r.eax >> 5) & 0x7;
        // EAX bits 25:14 hold `num_threads_sharing - 1` (Intel SDM leaf 4 /
        // `union _cpuid4_leaf_eax.num_threads_sharing`); +1 gives the count of
        // logical processors that share this cache, which `get_cache_id()` /
        // `__cache_cpumap_setup()` use to build shared_cpu_map.
        let threads_sharing = ((r.eax >> 14) & 0xfff) + 1;
        let line = (r.ebx & 0xfff) + 1;
        let partition = ((r.ebx >> 12) & 0x3ff) + 1;
        let ways = ((r.ebx >> 22) & 0x3ff) + 1;
        let sets = r.ecx + 1;
        let size = sets * line * partition * ways;
        leaves.push(
            CacheLeaf {
                level,
                ctype,
                size: Some(size),
                line_size: Some(line),
                sets: Some(sets),
                ways: Some(ways),
                partition: Some(partition),
                sharing: SharingScope::Private,
            }
            .with_arch_info_sharing(Some(threads_sharing)),
        );
        if leaves.len() >= 16 {
            break;
        }
    }
    leaves
}

/// A `CPUID` leaf 0x2 one-byte cache descriptor: which cache it describes and
/// its total size in bytes. Mirrors the cache rows of Linux's
/// `cpuid_0x2_table[256]` (`arch/x86/kernel/cpu/cpuid_0x2_table.c`); the TLB
/// descriptor rows are intentionally omitted (they carry no cache geometry).
#[cfg(target_arch = "x86_64")]
struct Leaf2Cache {
    level: u32,
    ctype: CacheType,
    /// Total cache size in bytes.
    size: u32,
    /// `coherency_line_size` in bytes, per the Intel SDM descriptor definition.
    line: u32,
}

/// Map a leaf 0x2 descriptor byte to its cache geometry, transcribed 1:1 from
/// the `CACHE_ENTRY(...)` rows of Linux's `cpuid_0x2_table[]`. `size` is in
/// bytes (Linux stores KiB via `/ SZ_1K`; we keep bytes and match [`CacheLeaf`]).
/// `line` is the coherency line size named in each table row's comment (the
/// Intel SDM descriptor definition), which Linux does not store but sysfs
/// exposes when known.
#[cfg(target_arch = "x86_64")]
fn leaf2_cache_descriptor(desc: u8) -> Option<Leaf2Cache> {
    use CacheType::{Data, Instruction, Unified};
    const K: u32 = 1024;
    const M: u32 = 1024 * 1024;
    let (level, ctype, size, line) = match desc {
        0x06 => (1, Instruction, 8 * K, 32),
        0x08 => (1, Instruction, 16 * K, 32),
        0x09 => (1, Instruction, 32 * K, 64),
        0x0a => (1, Data, 8 * K, 32),
        0x0c => (1, Data, 16 * K, 32),
        0x0d => (1, Data, 16 * K, 64),
        0x0e => (1, Data, 24 * K, 64),
        0x21 => (2, Unified, 256 * K, 64),
        0x22 => (3, Unified, 512 * K, 64),
        0x23 => (3, Unified, M, 64),
        0x25 => (3, Unified, 2 * M, 64),
        0x29 => (3, Unified, 4 * M, 64),
        0x2c => (1, Data, 32 * K, 64),
        0x30 => (1, Instruction, 32 * K, 64),
        0x39 => (2, Unified, 128 * K, 64),
        0x3a => (2, Unified, 192 * K, 64),
        0x3b => (2, Unified, 128 * K, 64),
        0x3c => (2, Unified, 256 * K, 64),
        0x3d => (2, Unified, 384 * K, 64),
        0x3e => (2, Unified, 512 * K, 64),
        0x3f => (2, Unified, 256 * K, 64),
        0x41 => (2, Unified, 128 * K, 32),
        0x42 => (2, Unified, 256 * K, 32),
        0x43 => (2, Unified, 512 * K, 32),
        0x44 => (2, Unified, M, 32),
        0x45 => (2, Unified, 2 * M, 32),
        0x46 => (3, Unified, 4 * M, 64),
        0x47 => (3, Unified, 8 * M, 64),
        0x48 => (2, Unified, 3 * M, 64),
        0x49 => (3, Unified, 4 * M, 64),
        0x4a => (3, Unified, 6 * M, 64),
        0x4b => (3, Unified, 8 * M, 64),
        0x4c => (3, Unified, 12 * M, 64),
        0x4d => (3, Unified, 16 * M, 64),
        0x4e => (2, Unified, 6 * M, 64),
        0x60 => (1, Data, 16 * K, 64),
        0x66 => (1, Data, 8 * K, 64),
        0x67 => (1, Data, 16 * K, 64),
        0x68 => (1, Data, 32 * K, 64),
        0x78 => (2, Unified, M, 64),
        0x79 => (2, Unified, 128 * K, 64),
        0x7a => (2, Unified, 256 * K, 64),
        0x7b => (2, Unified, 512 * K, 64),
        0x7c => (2, Unified, M, 64),
        0x7d => (2, Unified, 2 * M, 64),
        0x7f => (2, Unified, 512 * K, 64),
        0x80 => (2, Unified, 512 * K, 64),
        0x82 => (2, Unified, 256 * K, 32),
        0x83 => (2, Unified, 512 * K, 32),
        0x84 => (2, Unified, M, 32),
        0x85 => (2, Unified, 2 * M, 32),
        0x86 => (2, Unified, 512 * K, 64),
        0x87 => (2, Unified, M, 64),
        0xd0 => (3, Unified, 512 * K, 64),
        0xd1 => (3, Unified, M, 64),
        0xd2 => (3, Unified, 2 * M, 64),
        0xd6 => (3, Unified, M, 64),
        0xd7 => (3, Unified, 2 * M, 64),
        0xd8 => (3, Unified, 4 * M, 64),
        0xdc => (3, Unified, 2 * M, 64),
        0xdd => (3, Unified, 4 * M, 64),
        0xde => (3, Unified, 8 * M, 64),
        0xe2 => (3, Unified, 2 * M, 64),
        0xe3 => (3, Unified, 4 * M, 64),
        0xe4 => (3, Unified, 8 * M, 64),
        0xea => (3, Unified, 12 * M, 64),
        0xeb => (3, Unified, 18 * M, 64),
        0xec => (3, Unified, 24 * M, 64),
        _ => return None,
    };
    Some(Leaf2Cache {
        level,
        ctype,
        size,
        line,
    })
}

/// Legacy `CPUID` leaf 0x2 fallback, mirroring `intel_cacheinfo_0x2()`.
///
/// Leaf 0x2 returns four 32-bit registers of one-byte descriptors. AL (the low
/// byte of EAX on the first execution) is the number of times leaf 0x2 must be
/// executed to enumerate every descriptor, per the Intel SDM Vol. 2A CPUID
/// definition - not a validity flag. The leaf is therefore executed AL times and
/// the count byte itself is not a descriptor. A register whose top bit (bit 31)
/// is set carries no valid descriptors and is treated as NULL (`cpuid_leaf_0x2()`,
/// `struct leaf_0x2_reg.invalid`).
///
/// Each remaining descriptor byte is looked up in the cache table; Linux
/// *accumulates* sizes per category (L1I, L1D, L2, L3) and exposes only
/// level/type/size - leaf 0x2 carries no set/way counts, so those attributes are
/// omitted (hidden by `cache_default_attrs_is_visible()`). The coherency line
/// size named in each descriptor is emitted when known.
#[cfg(target_arch = "x86_64")]
fn read_cache_leaves_x86_leaf2() -> Vec<CacheLeaf> {
    // `CPUID.EAX=2` returns descriptor bytes across EAX/EBX/ECX/EDX.
    #[allow(unused_unsafe)]
    let r = unsafe { core::arch::x86_64::__cpuid(2) };

    // Intel requires the iteration count in AL to be 1; otherwise the leaf is
    // not usable and every descriptor is treated as NULL, matching Linux
    // `cpuid_leaf_0x2` (the legacy multi-iteration form is not used).
    if r.eax & 0xff != 0x01 {
        return Vec::new();
    }

    // Accumulate size (and remember the line size) per category, exactly as
    // `intel_cacheinfo_0x2` sums into l1i/l1d/l2/l3.
    #[derive(Clone, Copy, Default)]
    struct Acc {
        size: u32,
        line: u32,
    }
    let mut l1i = Acc::default();
    let mut l1d = Acc::default();
    let mut l2 = Acc::default();
    let mut l3 = Acc::default();

    for (reg_idx, reg) in [r.eax, r.ebx, r.ecx, r.edx].into_iter().enumerate() {
        // A register with its most-significant bit set holds no valid
        // descriptors (`struct leaf_0x2_reg.invalid`).
        if reg & 0x8000_0000 != 0 {
            continue;
        }
        for byte in 0..4 {
            // Skip the iteration-count byte (low byte of EAX only).
            if reg_idx == 0 && byte == 0 {
                continue;
            }
            let desc = ((reg >> (byte * 8)) & 0xff) as u8;
            if let Some(c) = leaf2_cache_descriptor(desc) {
                let acc = match (c.level, c.ctype) {
                    (1, CacheType::Instruction) => &mut l1i,
                    (1, CacheType::Data) => &mut l1d,
                    (2, _) => &mut l2,
                    _ => &mut l3,
                };
                acc.size += c.size;
                acc.line = c.line;
            }
        }
    }

    // Emit leaves in ascending level order (L1I, L1D, L2, L3), matching the
    // sysfs `index*` ordering; a category with zero accumulated size is absent.
    let mut leaves = Vec::new();
    let mut push = |acc: Acc, level: u32, ctype: CacheType| {
        if acc.size > 0 {
            // Leaf 0x2 carries no thread-sharing count, so fall back to the
            // level-only arch-info rule (L1 private, L2+ system-wide).
            leaves.push(
                CacheLeaf {
                    level,
                    ctype,
                    size: Some(acc.size),
                    line_size: (acc.line > 0).then_some(acc.line),
                    sets: None,
                    ways: None,
                    partition: None,
                    sharing: SharingScope::Private,
                }
                .with_arch_info_sharing(None),
            );
        }
    };
    push(l1i, 1, CacheType::Instruction);
    push(l1d, 1, CacheType::Data);
    push(l2, 2, CacheType::Unified);
    push(l3, 3, CacheType::Unified);
    leaves
}

/// aarch64: `CLIDR_EL1` gives the per-level cache type (Ctype: 1=I, 2=D, 3=separate I+D,
/// 4=unified); `CCSIDR_EL1` (after selecting the leaf via `CSSELR_EL1`) gives line
/// size/associativity/sets. Uses the CCIDX-wide `CCSIDR_EL1` layout when
/// `ID_AA64MMFR2_EL1.CCIDX` is set, else the 32-bit layout (ARM ARM D17.2.26).
#[cfg(target_arch = "aarch64")]
fn read_cache_leaves_aarch64() -> Vec<CacheLeaf> {
    use core::arch::asm;

    // CLIDR_EL1, ID_AA64MMFR2_EL1 and the CSSELR_EL1-select / CCSIDR_EL1-read /
    // CSSELR_EL1-restore sequence must all execute on one PE: CSSELR is a
    // per-PE selection register, so a migration between `msr csselr_el1` and
    // `mrs ccsidr_el1` would read an unselected leaf from another core and
    // restore this core's saved value onto that core. Hold preemption (and
    // therefore migration) off for the whole discovery so every leaf comes from
    // the same PE.
    let _guard = crate::sync::PreemptIrqSaveGuard::new();

    let clidr: u64;
    let ccidx_field: u64;
    unsafe {
        asm!("mrs {}, clidr_el1", out(reg) clidr, options(nomem, nostack, preserves_flags));
        asm!("mrs {}, id_aa64mmfr2_el1", out(reg) ccidx_field, options(nomem, nostack, preserves_flags));
    }
    let ccidx = ((ccidx_field >> 20) & 0xf) != 0;

    // Read CCSIDR for leaf (level, instruction-side?), restoring CSSELR afterwards.
    let read_ccsidr = |level: u32, instr: bool| -> u64 {
        let sel = ((level as u64 - 1) << 1) | (instr as u64);
        let saved: u64;
        let val: u64;
        unsafe {
            asm!("mrs {}, csselr_el1", out(reg) saved, options(nomem, nostack, preserves_flags));
            asm!("msr csselr_el1, {}", in(reg) sel, options(nomem, nostack, preserves_flags));
            asm!("isb", options(nomem, nostack, preserves_flags));
            asm!("mrs {}, ccsidr_el1", out(reg) val, options(nomem, nostack, preserves_flags));
            asm!("msr csselr_el1, {}", in(reg) saved, options(nomem, nostack, preserves_flags));
            asm!("isb", options(nomem, nostack, preserves_flags));
        }
        val
    };
    let geom = |ccsidr: u64| -> (u32, u32, u32) {
        let (assoc, sets) = if ccidx {
            (
                ((ccsidr >> 3) & 0x1f_ffff) as u32,
                ((ccsidr >> 32) & 0xff_ffff) as u32,
            )
        } else {
            (
                ((ccsidr >> 3) & 0x3ff) as u32,
                ((ccsidr >> 13) & 0x7fff) as u32,
            )
        };
        let line = 1u32 << (((ccsidr & 0x7) as u32) + 4);
        let ways = assoc + 1;
        let nsets = sets + 1;
        (line, nsets, ways)
    };
    let leaf = |level: u32, ctype: CacheType, instr: bool| -> CacheLeaf {
        let (line, sets, ways) = geom(read_ccsidr(level, instr));
        // CLIDR/CCSIDR carry no thread-sharing count; use the level-only
        // arch-info rule (L1 private, L2+ system-wide), matching the arm64
        // `use_arch_info` fallback in cacheinfo.c.
        CacheLeaf {
            level,
            ctype,
            size: Some(line * sets * ways),
            line_size: Some(line),
            sets: Some(sets),
            ways: Some(ways),
            partition: Some(1),
            sharing: SharingScope::Private,
        }
        .with_arch_info_sharing(None)
    };

    let mut leaves = Vec::new();
    for level in 1u32..=7 {
        let ctype = (clidr >> (3 * (level - 1))) & 0x7;
        match ctype {
            0 => break, // NoCache: highest level reached.
            1 => leaves.push(leaf(level, CacheType::Instruction, true)),
            2 => leaves.push(leaf(level, CacheType::Data, false)),
            3 => {
                // Separate I and D leaves at this level (data-side selects instr=false).
                leaves.push(leaf(level, CacheType::Data, false));
                leaves.push(leaf(level, CacheType::Instruction, true));
            }
            4 => leaves.push(leaf(level, CacheType::Unified, false)),
            _ => break,
        }
    }
    leaves
}

/// loongarch64: `CPUCFG` leaf 16 is the cache-present bitmap; leaves 17.. hold per-leaf
/// geometry (ways = field+1, sets = 1<<field, line = 1<<field). Mirrors
/// `arch/loongarch/mm/cache.c:cpu_cache_init()`.
#[cfg(target_arch = "loongarch64")]
fn read_cache_leaves_loongarch64() -> Vec<CacheLeaf> {
    use core::arch::asm;

    let cpucfg = |leaf: u32| -> u32 {
        let val: u32;
        unsafe {
            asm!("cpucfg {}, {}", out(reg) val, in(reg) leaf, options(nomem, nostack, preserves_flags));
        }
        val
    };

    let cfg16 = cpucfg(16);
    let mut leaves = Vec::new();
    let mut cfg_leaf = 0u32; // index into CPUCFG17.. as leaves are found.
    let push = |leaves: &mut Vec<CacheLeaf>, level: u32, ctype: CacheType, cfg_leaf: &mut u32| {
        let cfg1 = cpucfg(17 + *cfg_leaf);
        let ways = (cfg1 & 0xffff) + 1;
        let sets = 1u32 << ((cfg1 >> 16) & 0xff);
        let line = 1u32 << ((cfg1 >> 24) & 0x7f);
        // CPUCFG carries no thread-sharing count; use the level-only arch-info
        // rule (L1 private, L2+ system-wide).
        leaves.push(
            CacheLeaf {
                level,
                ctype,
                size: Some(ways * sets * line),
                line_size: Some(line),
                sets: Some(sets),
                ways: Some(ways),
                partition: Some(1),
                sharing: SharingScope::Private,
            }
            .with_arch_info_sharing(None),
        );
        *cfg_leaf += 1;
    };

    // L1: I/U (bit0), unified flag (bit1); D (bit2).
    if cfg16 & (1 << 0) != 0 {
        let ct = if cfg16 & (1 << 1) != 0 {
            CacheType::Unified
        } else {
            CacheType::Instruction
        };
        push(&mut leaves, 1, ct, &mut cfg_leaf);
    }
    if cfg16 & (1 << 2) != 0 {
        push(&mut leaves, 1, CacheType::Data, &mut cfg_leaf);
    }
    // L2 (>>3) and L3 (>>7): IU-present, IU-unify, ...; D-present.
    let mut config = cfg16 >> 3;
    for level in 2u32..=3 {
        if config == 0 {
            break;
        }
        if config & (1 << 0) != 0 {
            let ct = if config & (1 << 1) != 0 {
                CacheType::Unified
            } else {
                CacheType::Instruction
            };
            push(&mut leaves, level, ct, &mut cfg_leaf);
        }
        if config & (1 << 4) != 0 {
            push(&mut leaves, level, CacheType::Data, &mut cfg_leaf);
        }
        config >>= 7;
    }
    leaves
}

/// `/sys/devices/system/cpu/cpu<N>/cache/` - the real cache leaves enumerated for this CPU.
struct CpuCacheDir {
    fs: Arc<SimpleFs>,
    cpu: usize,
}

impl SimpleDirOps for CpuCacheDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let n = cpu_cache_leaves(self.cpu).len();
        Box::new((0..n).map(|i| Cow::Owned(format!("index{i}"))))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let index = name
            .strip_prefix("index")
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or(VfsError::NotFound)?;
        // Read *this CPU's* pinned leaves, not a live read of the executing PE.
        let leaves = cpu_cache_leaves(self.cpu);
        let leaf = *leaves.get(index).ok_or(VfsError::NotFound)?;
        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            self.fs.clone(),
            Arc::new(CpuCacheIndexDir {
                fs: self.fs.clone(),
                cpu: self.cpu,
                leaf,
            }),
        )))
    }
}

/// `/sys/devices/system/cpu/cpu<N>/cache/index<I>/` - one real cache leaf's attributes.
///
/// Only attributes whose value the hardware actually reported are exposed, matching Linux's
/// `cache_default_attrs_is_visible()`. `shared_cpu_map`/`shared_cpu_list` are rebuilt from the
/// leaf's [`SharingScope`]: aarch64 / loongarch64 follow the level-only `use_arch_info` rule
/// (L1 private, L2+ system-wide), while x86 honours its `num_threads_sharing` count (see
/// [`CpuCacheIndexDir::shared_mask`] and [`SharingScope`]).
struct CpuCacheIndexDir {
    fs: Arc<SimpleFs>,
    cpu: usize,
    leaf: CacheLeaf,
}

impl CpuCacheIndexDir {
    /// The set of CPUs sharing this cache, expanding the per-leaf
    /// [`SharingScope`] fixed at detection time (see that type for the Linux
    /// mapping):
    /// - [`Private`](SharingScope::Private) -> the owning CPU only (L1, or an
    ///   x86 leaf with `num_threads_sharing == 1`).
    /// - [`SystemWide`](SharingScope::SystemWide) -> all online CPUs, the
    ///   package domain: the aarch64 / loongarch64 L2+ rule from
    ///   `cache_leaves_are_shared()`, and equally any x86 cache shared beyond
    ///   its owner under this single-package topology.
    ///
    /// At `-smp 1` both scopes collapse to `cpu` alone (bit 0); the runtime
    /// distinction is only observable once more than one CPU is online.
    fn shared_mask(&self) -> String {
        match self.leaf.sharing {
            SharingScope::Private => cpu_bit_mask(self.cpu),
            SharingScope::SystemWide => cpu_hex_mask(),
            // Omitted from `child_names`/`lookup_child`, so never rendered; the
            // owner-only fallback keeps the mask well-formed if ever reached.
            SharingScope::Unknown => cpu_bit_mask(self.cpu),
        }
    }

    fn shared_list(&self) -> String {
        match self.leaf.sharing {
            SharingScope::Private => format!("{}", self.cpu),
            SharingScope::SystemWide => cpu_range_string(),
            SharingScope::Unknown => format!("{}", self.cpu),
        }
    }
}

impl SimpleDirOps for CpuCacheIndexDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        // level and type are always known; geometry only when the arch reported it. `id` is
        // omitted: like Linux, it is only exposed when firmware supplies a real per-cache
        // identifier (`CACHE_ID`), which no arch facility here provides.
        let mut names: Vec<Cow<'a, str>> =
            alloc::vec![Cow::Borrowed("level"), Cow::Borrowed("type"),];
        // `shared_cpu_map`/`shared_cpu_list` only when the exact sharing set is known - the owner
        // (Private) or a provably package-wide set (SystemWide). An [`Unknown`](SharingScope::Unknown)
        // leaf (an x86 subset we cannot pin to specific CPUs) omits them rather than fabricate.
        if !matches!(self.leaf.sharing, SharingScope::Unknown) {
            names.push(Cow::Borrowed("shared_cpu_map"));
            names.push(Cow::Borrowed("shared_cpu_list"));
        }
        if self.leaf.line_size.is_some() {
            names.push(Cow::Borrowed("coherency_line_size"));
        }
        if self.leaf.ways.is_some() {
            names.push(Cow::Borrowed("ways_of_associativity"));
        }
        if self.leaf.sets.is_some() {
            names.push(Cow::Borrowed("number_of_sets"));
        }
        if self.leaf.size.is_some() {
            names.push(Cow::Borrowed("size"));
        }
        if self.leaf.partition.is_some() {
            names.push(Cow::Borrowed("physical_line_partition"));
        }
        Box::new(names.into_iter())
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let leaf = self.leaf;
        // Resolve to an owned body up front so the read closure captures no borrowed `name`.
        // `size` is emitted as `%uK` (KiB) exactly like Linux's `size_show()`.
        let content = match name {
            "level" => format!("{}\n", leaf.level),
            "type" => format!("{}\n", leaf.ctype.as_str()),
            // Omitted (see `child_names`) when the exact sharing set is un-derivable.
            "shared_cpu_map" if !matches!(leaf.sharing, SharingScope::Unknown) => {
                format!("{}\n", self.shared_mask())
            }
            "shared_cpu_list" if !matches!(leaf.sharing, SharingScope::Unknown) => {
                format!("{}\n", self.shared_list())
            }
            "coherency_line_size" => format!("{}\n", leaf.line_size.ok_or(VfsError::NotFound)?),
            "ways_of_associativity" => format!("{}\n", leaf.ways.ok_or(VfsError::NotFound)?),
            "number_of_sets" => format!("{}\n", leaf.sets.ok_or(VfsError::NotFound)?),
            "size" => format!("{}K\n", leaf.size.ok_or(VfsError::NotFound)? / 1024),
            "physical_line_partition" => format!("{}\n", leaf.partition.ok_or(VfsError::NotFound)?),
            _ => return Err(VfsError::NotFound),
        };
        Ok(SimpleFile::new_regular(self.fs.clone(), move || Ok(content.clone())).into())
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
        // loongarch64 gates out both `cluster_*` and `die_*` below, so nothing is appended there.
        #[cfg_attr(target_arch = "loongarch64", allow(unused_mut))]
        let mut names: Vec<Cow<'a, str>> = alloc::vec![
            Cow::Borrowed("core_id"),
            Cow::Borrowed("physical_package_id"),
            Cow::Borrowed("core_cpus"),
            Cow::Borrowed("core_cpus_list"),
            Cow::Borrowed("thread_siblings"),
            Cow::Borrowed("thread_siblings_list"),
            // `core_siblings{,_list}` are in `topology.c`'s `bin_attrs[]` with no `#ifdef`, so every
            // arch exposes them; they are backed by `core_cpumask` - the package/socket domain,
            // the same mask `package_cpus` renders.
            Cow::Borrowed("core_siblings"),
            Cow::Borrowed("core_siblings_list"),
            Cow::Borrowed("package_cpus"),
            Cow::Borrowed("package_cpus_list"),
        ];
        // `cluster_*` gate behind Linux's `TOPOLOGY_CLUSTER_SYSFS` (include/linux/topology.h:183),
        // set when the arch defines `topology_cluster_id`/`topology_cluster_cpumask`: x86_64
        // (arch/x86/include/asm/topology.h) plus aarch64/riscv64 via include/linux/arch_topology.h;
        // loongarch64 defines neither, so it omits them.
        #[cfg(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        ))]
        names.extend(
            ["cluster_id", "cluster_cpus", "cluster_cpus_list"]
                .into_iter()
                .map(Cow::Borrowed),
        );
        // `die_*` gate behind `TOPOLOGY_DIE_SYSFS` (include/linux/topology.h:180), which needs
        // `topology_die_id`+`topology_die_cpumask` - defined only by x86_64 among the four targets
        // (arch/x86/include/asm/topology.h:147,201); aarch64/riscv64/loongarch64 define neither.
        #[cfg(target_arch = "x86_64")]
        names.extend(
            ["die_id", "die_cpus", "die_cpus_list"]
                .into_iter()
                .map(Cow::Borrowed),
        );
        Box::new(names.into_iter())
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
                }),
            ))),
            _ => Err(VfsError::NotFound),
        }
    }
}

/// `/sys/devices/system/cpu/cpu<N>/regs/identification/` — `midr_el1` for perf.
struct CpuIdRegsDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for CpuIdRegsDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(["midr_el1"].into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match name {
            "midr_el1" => Ok(SimpleFile::new_regular(self.fs.clone(), || {
                let midr = crate::perf::read_midr_el1();
                Ok(alloc::format!("{midr:016x}\n"))
            })
            .into()),
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

#[cfg(test)]
mod cache_sharing_tests {
    use super::*;

    // Regression guard for the shared_cpu_map cache-ABI fix (#1573). The pre-fix
    // code returned an owner-only scope for an x86 leaf with
    // `1 < num_threads_sharing < online`, under-reporting a genuinely-shared cache;
    // widening it to every CPU (SystemWide) would instead over-report a partial
    // cache as package-wide. The fix maps each leaf against the online count:
    // private (owner), package-wide (all online, provable), or - for a strict
    // subset whose members need APIC grouping this flat tree lacks - Unknown, so
    // `shared_cpu_map`/`shared_cpu_list` are omitted rather than fabricated. The
    // homogeneous -smp 4 QEMU the system test runs on only ever produces private
    // and all-online leaves, so this unit test is what pins the subset mapping.
    #[test]
    fn arch_info_scope_maps_sharing_against_online_count() {
        // 4 online CPUs is the -smp 4 system-suite package size.
        const ONLINE: u32 = 4;

        // L1, or a single-thread leaf at any level, is private.
        assert!(matches!(
            arch_info_scope(1, Some(1), ONLINE),
            SharingScope::Private
        ));
        assert!(matches!(
            arch_info_scope(1, None, ONLINE),
            SharingScope::Private
        ));
        assert!(matches!(
            arch_info_scope(2, Some(1), ONLINE),
            SharingScope::Private
        ));

        // Shared by every online CPU -> the whole package (provably SystemWide);
        // a count at or beyond the online set stays package-wide.
        assert!(matches!(
            arch_info_scope(3, Some(4), ONLINE),
            SharingScope::SystemWide
        ));
        assert!(matches!(
            arch_info_scope(3, Some(8), ONLINE),
            SharingScope::SystemWide
        ));

        // Shared by a strict subset (1 < n < online) -> Unknown: exact members are
        // un-derivable here, so the attribute is omitted, neither owner-only nor all.
        assert!(matches!(
            arch_info_scope(2, Some(2), ONLINE),
            SharingScope::Unknown
        ));
        assert!(matches!(
            arch_info_scope(2, Some(3), ONLINE),
            SharingScope::Unknown
        ));

        // Register-only path (no count) -> higher levels system-wide, L1 private.
        assert!(matches!(
            arch_info_scope(2, None, ONLINE),
            SharingScope::SystemWide
        ));
        assert!(matches!(
            arch_info_scope(3, None, ONLINE),
            SharingScope::SystemWide
        ));
    }

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

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

use crate::pseudofs::{
    DirMaker, DirMapping, NodeOpsMux, SimpleDir, SimpleDirOps, SimpleFile, SimpleFs,
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
        #[cfg(feature = "sg2002")]
        let names: &'static [&'static str] = &["drm", "graphics", "input", "pwm"];
        #[cfg(not(feature = "sg2002"))]
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
            #[cfg(feature = "sg2002")]
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
        #[cfg(feature = "plat-dyn")]
        let names: &'static [&'static str] = &["platform", "usb"];
        #[cfg(not(feature = "plat-dyn"))]
        let names: &'static [&'static str] = &["platform"];
        Box::new(names.iter().copied().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(NodeOpsMux::Dir(match name {
            "platform" => SimpleDir::new_maker(fs.clone(), Arc::new(PlatformBusClassDir)),
            #[cfg(feature = "plat-dyn")]
            "usb" => SimpleDir::new_maker(fs.clone(), Arc::new(DirMapping::new())),
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

// ========================================================================
// /sys/devices
// ========================================================================

struct DevicesDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for DevicesDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            ["platform", "system", "virtual"]
                .into_iter()
                .map(Cow::Borrowed),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let fs = self.fs.clone();
        Ok(NodeOpsMux::Dir(match name {
            "platform" => SimpleDir::new_maker(fs.clone(), Arc::new(PlatformBusDir { fs })),
            "system" => SimpleDir::new_maker(fs.clone(), Arc::new(SystemDir { fs })),
            "virtual" => SimpleDir::new_maker(fs.clone(), Arc::new(VirtualDir { fs })),
            _ => return Err(VfsError::NotFound),
        }))
    }
}

/// `/sys/devices/system/` — kernel topology subsystems.
struct SystemDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for SystemDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(["cpu"].into_iter().map(Cow::Borrowed))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match name {
            "cpu" => Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
                self.fs.clone(),
                Arc::new(SystemCpuDir {
                    fs: self.fs.clone(),
                }),
            ))),
            _ => Err(VfsError::NotFound),
        }
    }
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
        Box::new(["online"].into_iter().map(Cow::Borrowed))
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
            _ => Err(VfsError::NotFound),
        }
    }
}

fn cpu_range_string() -> String {
    let cpu_num = ax_runtime::hal::cpu_num();
    if cpu_num <= 1 {
        "0".to_owned()
    } else {
        format!("0-{}", cpu_num - 1)
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

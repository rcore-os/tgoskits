//! Wire structures and command numbers from Linux `sound/asound.h`.
//!
//! The structures in this module use `repr(C)` and integer storage. Userspace
//! values that are not structurally validated by Rust remain integers in the
//! wire type; callers must use the supplied constants and validation helpers
//! before acting on them.

use core::mem::size_of;

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// ALSA protocol version constructor.
pub const fn protocol_version(major: u32, minor: u32, micro: u32) -> u32 {
    ((major & 0xffff) << 16) | ((minor & 0xff) << 8) | (micro & 0xff)
}

/// Major component of an ALSA protocol version.
pub const fn protocol_major(version: u32) -> u32 {
    (version >> 16) & 0xffff
}

/// Minor component of an ALSA protocol version.
pub const fn protocol_minor(version: u32) -> u32 {
    (version >> 8) & 0xff
}

/// Micro component of an ALSA protocol version.
pub const fn protocol_micro(version: u32) -> u32 {
    version & 0xff
}

/// Returns whether two protocol versions cannot be used together.
pub const fn protocol_incompatible(kernel: u32, userspace: u32) -> bool {
    protocol_major(kernel) != protocol_major(userspace)
        || (protocol_major(kernel) == protocol_major(userspace)
            && protocol_minor(kernel) != protocol_minor(userspace))
}

/// Linux v6.12's PCM UAPI protocol version.
///
/// The parameter helpers in this crate implement the subset needed by
/// alsa-lib/alsa-utils 1.2.13; advertising this number does not claim support
/// for every newer PCM feature.
pub const SNDRV_PCM_VERSION: u32 = protocol_version(2, 0, 18);
pub const SNDRV_CTL_VERSION: u32 = protocol_version(2, 0, 9);
pub const SNDRV_PCM_SW_PARAMS_PROTO_2_0_12: u32 = protocol_version(2, 0, 12);

pub const SNDRV_PCM_STREAM_PLAYBACK: i32 = 0;
pub const SNDRV_PCM_STREAM_CAPTURE: i32 = 1;
pub const SNDRV_PCM_STREAM_LAST: i32 = SNDRV_PCM_STREAM_CAPTURE;

pub const SNDRV_PCM_CLASS_GENERIC: i32 = 0;
pub const SNDRV_PCM_CLASS_MULTI: i32 = 1;
pub const SNDRV_PCM_CLASS_MODEM: i32 = 2;
pub const SNDRV_PCM_CLASS_DIGITIZER: i32 = 3;
pub const SNDRV_PCM_CLASS_LAST: i32 = SNDRV_PCM_CLASS_DIGITIZER;
pub const SNDRV_PCM_SUBCLASS_GENERIC_MIX: i32 = 0;
pub const SNDRV_PCM_SUBCLASS_MULTI_MIX: i32 = 1;
pub const SNDRV_PCM_SUBCLASS_LAST: i32 = SNDRV_PCM_SUBCLASS_MULTI_MIX;

pub const SNDRV_PCM_ACCESS_MMAP_INTERLEAVED: u32 = 0;
pub const SNDRV_PCM_ACCESS_MMAP_NONINTERLEAVED: u32 = 1;
pub const SNDRV_PCM_ACCESS_MMAP_COMPLEX: u32 = 2;
pub const SNDRV_PCM_ACCESS_RW_INTERLEAVED: u32 = 3;
pub const SNDRV_PCM_ACCESS_RW_NONINTERLEAVED: u32 = 4;
pub const SNDRV_PCM_ACCESS_LAST: u32 = SNDRV_PCM_ACCESS_RW_NONINTERLEAVED;

pub const SNDRV_PCM_FORMAT_S8: u32 = 0;
pub const SNDRV_PCM_FORMAT_U8: u32 = 1;
pub const SNDRV_PCM_FORMAT_S16_LE: u32 = 2;
pub const SNDRV_PCM_FORMAT_U16_LE: u32 = 4;
pub const SNDRV_PCM_SUBFORMAT_STD: u32 = 0;

pub const SNDRV_PCM_STATE_OPEN: i32 = 0;
pub const SNDRV_PCM_STATE_SETUP: i32 = 1;
pub const SNDRV_PCM_STATE_PREPARED: i32 = 2;
pub const SNDRV_PCM_STATE_RUNNING: i32 = 3;
pub const SNDRV_PCM_STATE_XRUN: i32 = 4;
pub const SNDRV_PCM_STATE_DRAINING: i32 = 5;
pub const SNDRV_PCM_STATE_PAUSED: i32 = 6;
pub const SNDRV_PCM_STATE_SUSPENDED: i32 = 7;
pub const SNDRV_PCM_STATE_DISCONNECTED: i32 = 8;
pub const SNDRV_PCM_STATE_LAST: i32 = SNDRV_PCM_STATE_DISCONNECTED;

pub const SNDRV_PCM_TSTAMP_NONE: i32 = 0;
pub const SNDRV_PCM_TSTAMP_ENABLE: i32 = 1;
pub const SNDRV_PCM_TSTAMP_TYPE_GETTIMEOFDAY: u32 = 0;
pub const SNDRV_PCM_TSTAMP_TYPE_MONOTONIC: u32 = 1;
pub const SNDRV_PCM_TSTAMP_TYPE_MONOTONIC_RAW: u32 = 2;
pub const SNDRV_PCM_TSTAMP_TYPE_LAST: u32 = SNDRV_PCM_TSTAMP_TYPE_MONOTONIC_RAW;

pub const SNDRV_PCM_AUDIO_TSTAMP_TYPE_DEFAULT: u32 = 1;
pub const SNDRV_PCM_AUDIO_TSTAMP_TYPE_LINK: u32 = 2;
pub const SNDRV_PCM_AUDIO_TSTAMP_TYPE_LINK_ABSOLUTE: u32 = 3;
pub const SNDRV_PCM_AUDIO_TSTAMP_TYPE_LINK_ESTIMATED: u32 = 4;
pub const SNDRV_PCM_AUDIO_TSTAMP_TYPE_LINK_SYNCHRONIZED: u32 = 5;
pub const SNDRV_PCM_AUDIO_TSTAMP_TYPE_LAST: u32 = SNDRV_PCM_AUDIO_TSTAMP_TYPE_LINK_SYNCHRONIZED;

pub const SNDRV_PCM_INFO_MMAP: u32 = 0x0000_0001;
pub const SNDRV_PCM_INFO_MMAP_VALID: u32 = 0x0000_0002;
pub const SNDRV_PCM_INFO_DOUBLE: u32 = 0x0000_0004;
pub const SNDRV_PCM_INFO_BATCH: u32 = 0x0000_0010;
pub const SNDRV_PCM_INFO_SYNC_APPLPTR: u32 = 0x0000_0020;
pub const SNDRV_PCM_INFO_PERFECT_DRAIN: u32 = 0x0000_0040;
pub const SNDRV_PCM_INFO_INTERLEAVED: u32 = 0x0000_0100;
pub const SNDRV_PCM_INFO_NONINTERLEAVED: u32 = 0x0000_0200;
pub const SNDRV_PCM_INFO_COMPLEX: u32 = 0x0000_0400;
pub const SNDRV_PCM_INFO_BLOCK_TRANSFER: u32 = 0x0001_0000;
pub const SNDRV_PCM_INFO_OVERRANGE: u32 = 0x0002_0000;
pub const SNDRV_PCM_INFO_RESUME: u32 = 0x0004_0000;
pub const SNDRV_PCM_INFO_PAUSE: u32 = 0x0008_0000;
pub const SNDRV_PCM_INFO_HALF_DUPLEX: u32 = 0x0010_0000;
pub const SNDRV_PCM_INFO_JOINT_DUPLEX: u32 = 0x0020_0000;
pub const SNDRV_PCM_INFO_SYNC_START: u32 = 0x0040_0000;
pub const SNDRV_PCM_INFO_NO_PERIOD_WAKEUP: u32 = 0x0080_0000;
pub const SNDRV_PCM_INFO_HAS_WALL_CLOCK: u32 = 0x0100_0000;
pub const SNDRV_PCM_INFO_HAS_LINK_ATIME: u32 = 0x0100_0000;
pub const SNDRV_PCM_INFO_HAS_LINK_ABSOLUTE_ATIME: u32 = 0x0200_0000;
pub const SNDRV_PCM_INFO_HAS_LINK_ESTIMATED_ATIME: u32 = 0x0400_0000;
pub const SNDRV_PCM_INFO_HAS_LINK_SYNCHRONIZED_ATIME: u32 = 0x0800_0000;
pub const SNDRV_PCM_INFO_EXPLICIT_SYNC: u32 = 0x1000_0000;
pub const SNDRV_PCM_INFO_NO_REWINDS: u32 = 0x2000_0000;
pub const SNDRV_PCM_INFO_DRAIN_TRIGGER: u32 = 0x4000_0000;
pub const SNDRV_PCM_INFO_FIFO_IN_FRAMES: u32 = 0x8000_0000;

pub const SNDRV_PCM_HW_PARAMS_NORESAMPLE: u32 = 1 << 0;
pub const SNDRV_PCM_HW_PARAMS_EXPORT_BUFFER: u32 = 1 << 1;
pub const SNDRV_PCM_HW_PARAMS_NO_PERIOD_WAKEUP: u32 = 1 << 2;
pub const SNDRV_PCM_HW_PARAMS_NO_DRAIN_SILENCE: u32 = 1 << 3;

pub const SNDRV_PCM_SYNC_PTR_HWSYNC: u32 = 1 << 0;
pub const SNDRV_PCM_SYNC_PTR_APPL: u32 = 1 << 1;
pub const SNDRV_PCM_SYNC_PTR_AVAIL_MIN: u32 = 1 << 2;

pub const SNDRV_PCM_MMAP_OFFSET_DATA: u32 = 0x0000_0000;
pub const SNDRV_PCM_MMAP_OFFSET_STATUS_OLD: u32 = 0x8000_0000;
pub const SNDRV_PCM_MMAP_OFFSET_CONTROL_OLD: u32 = 0x8100_0000;
pub const SNDRV_PCM_MMAP_OFFSET_STATUS_NEW: u32 = 0x8200_0000;
pub const SNDRV_PCM_MMAP_OFFSET_CONTROL_NEW: u32 = 0x8300_0000;
pub const SNDRV_PCM_MMAP_OFFSET_STATUS: u32 = SNDRV_PCM_MMAP_OFFSET_STATUS_OLD;
pub const SNDRV_PCM_MMAP_OFFSET_CONTROL: u32 = SNDRV_PCM_MMAP_OFFSET_CONTROL_OLD;

pub const SNDRV_PCM_HW_PARAM_ACCESS: u32 = 0;
pub const SNDRV_PCM_HW_PARAM_FORMAT: u32 = 1;
pub const SNDRV_PCM_HW_PARAM_SUBFORMAT: u32 = 2;
pub const SNDRV_PCM_HW_PARAM_SAMPLE_BITS: u32 = 8;
pub const SNDRV_PCM_HW_PARAM_FRAME_BITS: u32 = 9;
pub const SNDRV_PCM_HW_PARAM_CHANNELS: u32 = 10;
pub const SNDRV_PCM_HW_PARAM_RATE: u32 = 11;
pub const SNDRV_PCM_HW_PARAM_PERIOD_TIME: u32 = 12;
pub const SNDRV_PCM_HW_PARAM_PERIOD_SIZE: u32 = 13;
pub const SNDRV_PCM_HW_PARAM_PERIOD_BYTES: u32 = 14;
pub const SNDRV_PCM_HW_PARAM_PERIODS: u32 = 15;
pub const SNDRV_PCM_HW_PARAM_BUFFER_TIME: u32 = 16;
pub const SNDRV_PCM_HW_PARAM_BUFFER_SIZE: u32 = 17;
pub const SNDRV_PCM_HW_PARAM_BUFFER_BYTES: u32 = 18;
pub const SNDRV_PCM_HW_PARAM_TICK_TIME: u32 = 19;

pub const SNDRV_PCM_HW_PARAM_FIRST_MASK: u32 = SNDRV_PCM_HW_PARAM_ACCESS;
pub const SNDRV_PCM_HW_PARAM_LAST_MASK: u32 = SNDRV_PCM_HW_PARAM_SUBFORMAT;
pub const SNDRV_PCM_HW_PARAM_FIRST_INTERVAL: u32 = SNDRV_PCM_HW_PARAM_SAMPLE_BITS;
pub const SNDRV_PCM_HW_PARAM_LAST_INTERVAL: u32 = SNDRV_PCM_HW_PARAM_TICK_TIME;
pub const SNDRV_PCM_HW_PARAM_MASK_COUNT: usize = 3;
pub const SNDRV_PCM_HW_PARAM_INTERVAL_COUNT: usize = 12;

const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    ((dir & 0x3) << 30)
        | (((size as u32) & 0x3fff) << 16)
        | (((ty as u32) & 0xff) << 8)
        | ((nr as u32) & 0xff)
}

const fn ior<T>(ty: u8, nr: u8) -> u32 {
    ioc(2, ty, nr, size_of::<T>())
}

const fn iow<T>(ty: u8, nr: u8) -> u32 {
    ioc(1, ty, nr, size_of::<T>())
}

const fn iowr<T>(ty: u8, nr: u8) -> u32 {
    ioc(3, ty, nr, size_of::<T>())
}

const fn io(ty: u8, nr: u8) -> u32 {
    ioc(0, ty, nr, 0)
}

/// A 64-bit Linux timespec. Both fields are signed 64-bit integers on x86_64.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct LinuxTimespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

/// Interleaved sample bit mask.
pub const SNDRV_MASK_MAX: usize = 256;
pub const SNDRV_MASK_WORDS: usize = SNDRV_MASK_MAX / 32;

/// The fixed-size bit set used for access, format, and subformat parameters.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmMask {
    pub bits: [u32; SNDRV_MASK_WORDS],
}

impl PcmMask {
    pub const fn empty() -> Self {
        Self {
            bits: [0; SNDRV_MASK_WORDS],
        }
    }

    pub const fn full() -> Self {
        Self {
            bits: [u32::MAX; SNDRV_MASK_WORDS],
        }
    }

    pub const fn single(bit: u32) -> Self {
        let mut mask = Self::empty();
        let _ = mask.insert(bit);
        mask
    }

    pub const fn is_empty(self) -> bool {
        let mut i = 0;
        while i < SNDRV_MASK_WORDS {
            if self.bits[i] != 0 {
                return false;
            }
            i += 1;
        }
        true
    }

    pub const fn count(self) -> u32 {
        let mut count = 0;
        let mut i = 0;
        while i < SNDRV_MASK_WORDS {
            count += self.bits[i].count_ones();
            i += 1;
        }
        count
    }

    pub const fn has(self, bit: u32) -> bool {
        if bit >= SNDRV_MASK_MAX as u32 {
            return false;
        }
        let word = (bit / 32) as usize;
        let lane = 1u32 << (bit % 32);
        self.bits[word] & lane != 0
    }

    /// Inserts a bit and returns whether the set changed.
    pub const fn insert(&mut self, bit: u32) -> bool {
        if bit >= SNDRV_MASK_MAX as u32 {
            return false;
        }
        let word = (bit / 32) as usize;
        let lane = 1u32 << (bit % 32);
        let changed = self.bits[word] & lane == 0;
        self.bits[word] |= lane;
        changed
    }

    /// Removes a bit and returns whether the set changed.
    pub const fn remove(&mut self, bit: u32) -> bool {
        if bit >= SNDRV_MASK_MAX as u32 {
            return false;
        }
        let word = (bit / 32) as usize;
        let lane = 1u32 << (bit % 32);
        let changed = self.bits[word] & lane != 0;
        self.bits[word] &= !lane;
        changed
    }

    pub const fn intersection(self, other: Self) -> Self {
        let mut result = Self::empty();
        let mut i = 0;
        while i < SNDRV_MASK_WORDS {
            result.bits[i] = self.bits[i] & other.bits[i];
            i += 1;
        }
        result
    }

    pub const fn union(self, other: Self) -> Self {
        let mut result = Self::empty();
        let mut i = 0;
        while i < SNDRV_MASK_WORDS {
            result.bits[i] = self.bits[i] | other.bits[i];
            i += 1;
        }
        result
    }

    pub const fn first(self) -> Option<u32> {
        let mut i = 0;
        while i < SNDRV_MASK_WORDS {
            if self.bits[i] != 0 {
                return Some((i as u32 * 32) + self.bits[i].trailing_zeros());
            }
            i += 1;
        }
        None
    }
}

pub const SNDRV_INTERVAL_OPENMIN: u32 = 1 << 0;
pub const SNDRV_INTERVAL_OPENMAX: u32 = 1 << 1;
pub const SNDRV_INTERVAL_INTEGER: u32 = 1 << 2;
pub const SNDRV_INTERVAL_EMPTY: u32 = 1 << 3;
pub const SNDRV_INTERVAL_KNOWN_FLAGS: u32 =
    SNDRV_INTERVAL_OPENMIN | SNDRV_INTERVAL_OPENMAX | SNDRV_INTERVAL_INTEGER | SNDRV_INTERVAL_EMPTY;

/// A closed integer interval in the ALSA wire format.
///
/// Linux represents `openmin`, `openmax`, `integer`, and `empty` as bitfields.
/// They are stored here as `flags` so no invalid Rust enum can be produced from
/// userspace bytes. `normalized()` removes open endpoints and ignores unknown
/// flag bits.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq)]
pub struct PcmInterval {
    pub min: u32,
    pub max: u32,
    pub flags: u32,
}

impl Default for PcmInterval {
    fn default() -> Self {
        Self::empty()
    }
}

impl PcmInterval {
    pub const fn new(min: u32, max: u32) -> Self {
        if min > max {
            Self {
                min: 0,
                max: 0,
                flags: SNDRV_INTERVAL_INTEGER | SNDRV_INTERVAL_EMPTY,
            }
        } else {
            Self {
                min,
                max,
                flags: SNDRV_INTERVAL_INTEGER,
            }
        }
    }

    pub const fn point(value: u32) -> Self {
        Self::new(value, value)
    }

    pub const fn empty() -> Self {
        Self {
            min: 0,
            max: 0,
            flags: SNDRV_INTERVAL_INTEGER | SNDRV_INTERVAL_EMPTY,
        }
    }

    pub const fn full() -> Self {
        Self::new(0, u32::MAX)
    }

    pub const fn positive() -> Self {
        Self::new(1, u32::MAX)
    }

    /// Constructs a wire interval while retaining the raw flag field.
    pub const fn from_raw(min: u32, max: u32, flags: u32) -> Self {
        Self { min, max, flags }
    }

    /// Decodes the bitfield representation into the integer interval it names.
    ///
    /// Unknown flag bits are ignored. Open endpoints are advanced/retreated by
    /// one because all ALSA hardware parameters are integral.
    pub const fn normalized(self) -> Self {
        if self.flags & SNDRV_INTERVAL_EMPTY != 0 {
            return Self::empty();
        }

        let (min, min_ok) = if self.flags & SNDRV_INTERVAL_OPENMIN != 0 {
            match self.min.checked_add(1) {
                Some(value) => (value, true),
                None => (0, false),
            }
        } else {
            (self.min, true)
        };
        let (max, max_ok) = if self.flags & SNDRV_INTERVAL_OPENMAX != 0 {
            match self.max.checked_sub(1) {
                Some(value) => (value, true),
                None => (0, false),
            }
        } else {
            (self.max, true)
        };

        if !min_ok || !max_ok {
            return Self::empty();
        }
        if min > max {
            return Self::empty();
        }
        Self::new(min, max)
    }

    pub const fn is_empty(self) -> bool {
        self.normalized().flags & SNDRV_INTERVAL_EMPTY != 0
    }

    pub const fn is_singleton(self) -> bool {
        let value = self.normalized();
        !value.is_empty() && value.min == value.max
    }

    pub const fn exact(self) -> Option<u32> {
        let value = self.normalized();
        if value.is_empty() || value.min != value.max {
            None
        } else {
            Some(value.min)
        }
    }

    pub const fn contains(self, value: u32) -> bool {
        let range = self.normalized();
        !range.is_empty() && range.min <= value && value <= range.max
    }

    pub const fn intersection(self, other: Self) -> Self {
        let left = self.normalized();
        let right = other.normalized();
        if left.is_empty() || right.is_empty() {
            return Self::empty();
        }
        let min = if left.min > right.min {
            left.min
        } else {
            right.min
        };
        let max = if left.max < right.max {
            left.max
        } else {
            right.max
        };
        Self::new(min, max)
    }
}

/// PCM device information (`struct snd_pcm_info`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq)]
pub struct PcmInfo {
    pub device: u32,
    pub subdevice: u32,
    pub stream: i32,
    pub card: i32,
    pub id: [u8; 64],
    pub name: [u8; 80],
    pub subname: [u8; 32],
    pub dev_class: i32,
    pub dev_subclass: i32,
    pub subdevices_count: u32,
    pub subdevices_avail: u32,
    /// Deprecated synchronization-ID storage renamed `pad1` in Linux v6.12.
    pub pad1: [u8; 16],
    pub reserved: [u8; 64],
}

/// Control-card information (`struct snd_ctl_card_info`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq)]
pub struct CtlCardInfo {
    pub card: i32,
    pub pad: i32,
    pub id: [u8; 16],
    pub driver: [u8; 16],
    pub name: [u8; 32],
    pub longname: [u8; 80],
    pub reserved: [u8; 16],
    pub mixername: [u8; 80],
    pub components: [u8; 128],
}

/// PCM hardware-parameter description (`struct snd_pcm_hw_params`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq)]
pub struct PcmHwParams {
    pub flags: u32,
    pub masks: [PcmMask; SNDRV_PCM_HW_PARAM_MASK_COUNT],
    pub mres: [PcmMask; 5],
    pub intervals: [PcmInterval; SNDRV_PCM_HW_PARAM_INTERVAL_COUNT],
    pub ires: [PcmInterval; 9],
    /// Bit set of masks supplied by userspace.
    pub rmask: u32,
    /// Bit set of parameters changed by refinement.
    pub cmask: u32,
    /// Information flags returned for the selected setup.
    pub info: u32,
    /// Most significant sample bits used by the selected format.
    pub msbits: u32,
    /// Sample-rate numerator.
    pub rate_num: u32,
    /// Sample-rate denominator.
    pub rate_den: u32,
    /// Hardware FIFO size in frames.
    pub fifo_size: u64,
    /// Synchronization ID reported by the card/driver.
    pub sync: [u8; 16],
    pub reserved: [u8; 48],
}

/// PCM software parameters (`struct snd_pcm_sw_params`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq)]
pub struct PcmSwParams {
    /// Timestamp mode (`SNDRV_PCM_TSTAMP_*`).
    pub tstamp_mode: i32,
    /// Period step in periods.
    pub period_step: u32,
    /// Obsolete sleep duration in ticks.
    pub sleep_min: u32,
    pub pad: [u8; 4],
    /// Minimum available frames for a wakeup.
    pub avail_min: u64,
    /// Obsolete transfer alignment in frames.
    pub xfer_align: u64,
    /// Minimum available frames that trigger automatic start.
    pub start_threshold: u64,
    /// Available-frame threshold that stops the stream.
    pub stop_threshold: u64,
    /// Minimum distance from silence in frames.
    pub silence_threshold: u64,
    /// Silence block size in frames.
    pub silence_size: u64,
    /// Pointer wrap boundary in frames.
    pub boundary: u64,
    /// Software-parameter protocol version.
    pub proto: u32,
    /// Timestamp type (`SNDRV_PCM_TSTAMP_TYPE_*`).
    pub tstamp_type: u32,
    pub reserved: [u8; 56],
}

/// PCM status (`struct snd_pcm_status`).
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmStatus {
    pub state: i32,
    /// Explicit padding before the first 64-bit timespec.
    pub pad1: [u8; 4],
    pub trigger_tstamp: LinuxTimespec,
    pub tstamp: LinuxTimespec,
    /// Application pointer in frames.
    pub appl_ptr: u64,
    /// Hardware pointer in frames.
    pub hw_ptr: u64,
    /// Delay in frames.
    pub delay: i64,
    /// Available frames.
    pub avail: u64,
    /// Maximum available frames observed since the last status.
    pub avail_max: u64,
    /// ADC overrange detections since the last status.
    pub overrange: u64,
    pub suspended_state: i32,
    pub audio_tstamp_data: u32,
    pub audio_tstamp: LinuxTimespec,
    pub driver_tstamp: LinuxTimespec,
    /// Audio timestamp accuracy in nanoseconds.
    pub audio_tstamp_accuracy: u32,
    pub reserved: [u8; 20],
}

/// The traditional mmap status layout used by 64-bit x86_64 ioctls.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmMmapStatus {
    pub state: i32,
    pub pad1: i32,
    /// Hardware pointer in frames.
    pub hw_ptr: u64,
    pub tstamp: LinuxTimespec,
    pub suspended_state: i32,
    pub pad2: [u8; 4],
    pub audio_tstamp: LinuxTimespec,
}

/// The explicit 64-bit-time mmap status layout.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmMmapStatus64 {
    pub state: i32,
    pub pad1: u32,
    /// Hardware pointer in frames.
    pub hw_ptr: u64,
    pub tstamp: LinuxTimespec,
    pub suspended_state: i32,
    pub pad3: u32,
    pub audio_tstamp: LinuxTimespec,
}

/// The traditional mmap control layout.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmMmapControl {
    /// Application pointer in frames.
    pub appl_ptr: u64,
    /// Minimum available frames for a wakeup.
    pub avail_min: u64,
}

/// The explicit 64-bit-time mmap control layout.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmMmapControl64 {
    /// Application pointer in frames.
    pub appl_ptr: u64,
    /// Minimum available frames for a wakeup.
    pub avail_min: u64,
}

/// Per-channel mmap layout information (`struct snd_pcm_channel_info`).
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmChannelInfo {
    pub channel: u32,
    pub pad: [u8; 4],
    /// Mmap offset in bytes.
    pub offset: i64,
    /// Offset in bits to the first sample.
    pub first: u32,
    /// Distance between samples in bits.
    pub step: u32,
}

impl Default for PcmInfo {
    fn default() -> Self {
        Self {
            device: 0,
            subdevice: 0,
            stream: 0,
            card: 0,
            id: [0; 64],
            name: [0; 80],
            subname: [0; 32],
            dev_class: 0,
            dev_subclass: 0,
            subdevices_count: 0,
            subdevices_avail: 0,
            pad1: [0; 16],
            reserved: [0; 64],
        }
    }
}

impl Default for CtlCardInfo {
    fn default() -> Self {
        Self {
            card: 0,
            pad: 0,
            id: [0; 16],
            driver: [0; 16],
            name: [0; 32],
            longname: [0; 80],
            reserved: [0; 16],
            mixername: [0; 80],
            components: [0; 128],
        }
    }
}

impl Default for PcmHwParams {
    fn default() -> Self {
        Self {
            flags: 0,
            masks: [PcmMask::empty(); SNDRV_PCM_HW_PARAM_MASK_COUNT],
            mres: [PcmMask::empty(); 5],
            intervals: [PcmInterval::empty(); SNDRV_PCM_HW_PARAM_INTERVAL_COUNT],
            ires: [PcmInterval::empty(); 9],
            rmask: 0,
            cmask: 0,
            info: 0,
            msbits: 0,
            rate_num: 0,
            rate_den: 0,
            fifo_size: 0,
            sync: [0; 16],
            reserved: [0; 48],
        }
    }
}

impl Default for PcmSwParams {
    fn default() -> Self {
        Self {
            tstamp_mode: 0,
            period_step: 0,
            sleep_min: 0,
            pad: [0; 4],
            avail_min: 0,
            xfer_align: 0,
            start_threshold: 0,
            stop_threshold: 0,
            silence_threshold: 0,
            silence_size: 0,
            boundary: 0,
            proto: 0,
            tstamp_type: 0,
            reserved: [0; 56],
        }
    }
}

impl Default for PcmSyncControlStorage {
    fn default() -> Self {
        Self {
            control: PcmMmapControl::default(),
            reserved: [0; 48],
        }
    }
}

impl Default for PcmSyncControlStorage64 {
    fn default() -> Self {
        Self {
            control: PcmMmapControl64::default(),
            reserved: [0; 48],
        }
    }
}

/// The 64-byte status union member from `struct __snd_pcm_sync_ptr`.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmSyncStatusStorage {
    pub status: PcmMmapStatus,
    pub reserved: [u8; 8],
}

/// The 64-byte control union member from `struct __snd_pcm_sync_ptr`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq)]
pub struct PcmSyncControlStorage {
    pub control: PcmMmapControl,
    pub reserved: [u8; 48],
}

/// The 64-byte control union member for the explicit 64-bit-time request.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq)]
pub struct PcmSyncControlStorage64 {
    pub control: PcmMmapControl64,
    pub reserved: [u8; 48],
}

/// The traditional `SYNC_PTR` request.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmSyncPtr {
    pub flags: u32,
    pub pad: [u8; 4],
    pub s: PcmSyncStatusStorage,
    pub c: PcmSyncControlStorage,
}

/// The explicit 64-bit-time `SYNC_PTR64` request.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmSyncPtr64 {
    pub flags: u32,
    pub pad: [u8; 4],
    pub s: PcmSyncStatusStorage64,
    pub c: PcmSyncControlStorage64,
}

/// The 64-byte status union member for the explicit 64-bit-time request.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmSyncStatusStorage64 {
    pub status: PcmMmapStatus64,
    pub reserved: [u8; 8],
}

/// A frame transfer request (`struct snd_xferi`).
///
/// `buf` is an unvalidated userspace address, not a Rust reference.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmXferi {
    /// Signed frame count written by the kernel.
    pub result: i64,
    /// Userspace buffer address.
    pub buf: u64,
    /// Number of frames requested by userspace.
    pub frames: u64,
}

/// A non-interleaved frame transfer request (`struct snd_xfern`).
///
/// `bufs` is an unvalidated userspace address of an array of buffer addresses.
#[repr(C)]
#[derive(
    Clone, Copy, Debug, Default, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, PartialEq,
)]
pub struct PcmXfern {
    pub result: i64,
    pub bufs: u64,
    pub frames: u64,
}

pub const SNDRV_PCM_IOCTL_PVERSION: u32 = ior::<i32>(b'A', 0x00);
pub const SNDRV_PCM_IOCTL_INFO: u32 = ior::<PcmInfo>(b'A', 0x01);
pub const SNDRV_PCM_IOCTL_TSTAMP: u32 = iow::<i32>(b'A', 0x02);
pub const SNDRV_PCM_IOCTL_TTSTAMP: u32 = iow::<i32>(b'A', 0x03);
pub const SNDRV_PCM_IOCTL_USER_PVERSION: u32 = iow::<i32>(b'A', 0x04);
pub const SNDRV_PCM_IOCTL_HW_REFINE: u32 = iowr::<PcmHwParams>(b'A', 0x10);
pub const SNDRV_PCM_IOCTL_HW_PARAMS: u32 = iowr::<PcmHwParams>(b'A', 0x11);
pub const SNDRV_PCM_IOCTL_HW_FREE: u32 = io(b'A', 0x12);
pub const SNDRV_PCM_IOCTL_SW_PARAMS: u32 = iowr::<PcmSwParams>(b'A', 0x13);
pub const SNDRV_PCM_IOCTL_STATUS: u32 = ior::<PcmStatus>(b'A', 0x20);
pub const SNDRV_PCM_IOCTL_DELAY: u32 = ior::<i64>(b'A', 0x21);
pub const SNDRV_PCM_IOCTL_HWSYNC: u32 = io(b'A', 0x22);
pub const SNDRV_PCM_IOCTL_SYNC_PTR: u32 = iowr::<PcmSyncPtr>(b'A', 0x23);
pub const SNDRV_PCM_IOCTL_SYNC_PTR64: u32 = iowr::<PcmSyncPtr64>(b'A', 0x23);
pub const SNDRV_PCM_IOCTL_STATUS_EXT: u32 = iowr::<PcmStatus>(b'A', 0x24);
pub const SNDRV_PCM_IOCTL_CHANNEL_INFO: u32 = ior::<PcmChannelInfo>(b'A', 0x32);
pub const SNDRV_PCM_IOCTL_PREPARE: u32 = io(b'A', 0x40);
pub const SNDRV_PCM_IOCTL_RESET: u32 = io(b'A', 0x41);
pub const SNDRV_PCM_IOCTL_START: u32 = io(b'A', 0x42);
pub const SNDRV_PCM_IOCTL_DROP: u32 = io(b'A', 0x43);
pub const SNDRV_PCM_IOCTL_DRAIN: u32 = io(b'A', 0x44);
pub const SNDRV_PCM_IOCTL_PAUSE: u32 = iow::<i32>(b'A', 0x45);
pub const SNDRV_PCM_IOCTL_XRUN: u32 = io(b'A', 0x48);
pub const SNDRV_PCM_IOCTL_WRITEI_FRAMES: u32 = iow::<PcmXferi>(b'A', 0x50);
pub const SNDRV_PCM_IOCTL_READI_FRAMES: u32 = ior::<PcmXferi>(b'A', 0x51);
pub const SNDRV_PCM_IOCTL_WRITEN_FRAMES: u32 = iow::<PcmXfern>(b'A', 0x52);
pub const SNDRV_PCM_IOCTL_READN_FRAMES: u32 = ior::<PcmXfern>(b'A', 0x53);

pub const SNDRV_CTL_IOCTL_PVERSION: u32 = ior::<i32>(b'U', 0x00);
pub const SNDRV_CTL_IOCTL_CARD_INFO: u32 = ior::<CtlCardInfo>(b'U', 0x01);
pub const SNDRV_CTL_IOCTL_PCM_NEXT_DEVICE: u32 = ior::<i32>(b'U', 0x30);
pub const SNDRV_CTL_IOCTL_PCM_INFO: u32 = iowr::<PcmInfo>(b'U', 0x31);
pub const SNDRV_CTL_IOCTL_PCM_PREFER_SUBDEVICE: u32 = iow::<i32>(b'U', 0x32);

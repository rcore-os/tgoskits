//! Linux wireless-extensions (WE) `ioctl` support for socket fds.
//!
//! Implements the small subset of the wireless extensions a userspace program
//! (or the on-device HTTP control server) needs to switch a Wi-Fi interface
//! between Station and SoftAP at runtime:
//!
//! - `SIOCSIWMODE`    — stage the target mode (Managed/STA vs Master/AP).
//! - `SIOCSIWESSID`   — stage the SSID.
//! - `SIOCSIWENCODEEXT` — stage the passphrase (STA) / open (AP).
//! - `SIOCSIWFREQ`    — stage the channel (AP only).
//! - `SIOCSIWCOMMIT`  — atomically apply the staged config via
//!   [`ax_net::reconfigure_wifi`] (link-layer teardown + switch + IP/DHCP role).
//!
//! The `SIOCSIW*` setters never touch hardware; they only stage into a
//! per-interface pending config. `SIOCSIWCOMMIT` performs the whole transition
//! in one shot. This matches the "stage then commit" semantics chosen for this
//! driver and keeps the switch atomic from the caller's point of view.

use alloc::{string::String, vec::Vec};
use core::mem::MaybeUninit;

use ax_errno::{AxError, AxResult};
use spin::Mutex;
use starry_vm::{vm_read_slice, vm_write_slice};

// ---------------------------------------------------------------------------
// Wireless-extensions ioctl numbers (not provided by linux_raw_sys).
// These are the fixed values from <linux/wireless.h>.
// ---------------------------------------------------------------------------

pub const SIOCSIWCOMMIT: u32 = 0x8B00;
pub const SIOCSIWFREQ: u32 = 0x8B04;
pub const SIOCSIWMODE: u32 = 0x8B06;
pub const SIOCSIWESSID: u32 = 0x8B1A;
pub const SIOCSIWENCODEEXT: u32 = 0x8B34;

/// `iw_mode` values from <linux/wireless.h>.
const IW_MODE_INFRA: u32 = 2; // Managed / Station
const IW_MODE_MASTER: u32 = 3; // Master / Access Point

/// Offsets within `struct iwreq` (size 32 on both 32/64-bit: 16-byte ifrn_name
/// union followed by a 16-byte `union iwreq_data`).
const IWREQ_NAME_LEN: usize = 16;
const IWREQ_DATA_OFFSET: usize = 16;

/// Max SSID length per the spec.
const IW_ESSID_MAX_SIZE: usize = 32;

/// Max passphrase we accept (WPA2 PSK is <= 63 chars).
const MAX_PASSPHRASE: usize = 63;

/// Board SoftAP addressing policy applied on a switch to Master mode.
///
/// Mirrors the boot-time SoftAP policy the board attaches in
/// `ax-driver`'s aic8800 probe; kept here so a runtime switch to AP lands on
/// the same subnet as the boot default.
const AP_SERVER_IP: [u8; 4] = [192, 168, 50, 1];
const AP_CLIENT_IP: [u8; 4] = [192, 168, 50, 2];
const AP_PREFIX_LEN: u8 = 24;
const AP_CHANNEL_DEFAULT: u8 = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum StagedMode {
    Station,
    AccessPoint,
}

/// Per-interface staged wireless config, applied on `SIOCSIWCOMMIT`.
#[derive(Clone)]
struct Pending {
    ifname: String,
    mode: Option<StagedMode>,
    ssid: Option<Vec<u8>>,
    passphrase: Option<String>,
    channel: Option<u8>,
}

impl Pending {
    fn new(ifname: String) -> Self {
        Self {
            ifname,
            mode: None,
            ssid: None,
            passphrase: None,
            channel: None,
        }
    }
}

/// Staged wireless config, keyed by interface name.
///
/// Wireless-extensions state is per-netdev in Linux (not per-fd): any socket fd
/// can stage and commit for a given interface. We mirror that with a global
/// table rather than per-`Socket` state.
static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());

fn with_pending<R>(ifname: &str, f: impl FnOnce(&mut Pending) -> R) -> R {
    let mut table = PENDING.lock();
    if let Some(idx) = table.iter().position(|p| p.ifname == ifname) {
        f(&mut table[idx])
    } else {
        table.push(Pending::new(ifname.into()));
        let last = table.len() - 1;
        f(&mut table[last])
    }
}

fn take_pending(ifname: &str) -> Option<Pending> {
    let mut table = PENDING.lock();
    table
        .iter()
        .position(|p| p.ifname == ifname)
        .map(|idx| table.swap_remove(idx))
}

// ---------------------------------------------------------------------------
// iwreq parsing helpers
// ---------------------------------------------------------------------------

fn read_user_array<const N: usize>(ptr: *const u8) -> AxResult<[u8; N]> {
    let mut buf = [MaybeUninit::<u8>::uninit(); N];
    vm_read_slice(ptr, &mut buf)?;
    Ok(buf.map(|v| unsafe { v.assume_init() }))
}

fn read_ifname(arg: usize) -> AxResult<String> {
    let buf = read_user_array::<IWREQ_NAME_LEN>(arg as *const u8)?;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..end].to_vec()).map_err(|_| AxError::InvalidInput)
}

/// Reads the 16-byte `union iwreq_data` payload following the name.
fn read_iwreq_data(arg: usize) -> AxResult<[u8; 16]> {
    read_user_array::<16>((arg + IWREQ_DATA_OFFSET) as *const u8)
}

/// Reads a length-prefixed userspace buffer described by an `iw_point`
/// (`{ void* pointer; u16 length; u16 flags; }`) embedded in `iwreq_data`.
fn read_iw_point(arg: usize, max: usize) -> AxResult<Vec<u8>> {
    let data = read_iwreq_data(arg)?;
    let ptr = usize::from_ne_bytes(
        data[..core::mem::size_of::<usize>()]
            .try_into()
            .map_err(|_| AxError::InvalidInput)?,
    );
    let len = u16::from_ne_bytes([data[8], data[9]]) as usize;
    if ptr == 0 || len == 0 {
        return Ok(Vec::new());
    }
    if len > max {
        return Err(AxError::InvalidInput);
    }
    let mut buf = alloc::vec![MaybeUninit::<u8>::uninit(); len];
    vm_read_slice(ptr as *const u8, &mut buf)?;
    Ok(buf.into_iter().map(|v| unsafe { v.assume_init() }).collect())
}

// ---------------------------------------------------------------------------
// ioctl entry
// ---------------------------------------------------------------------------

/// Returns `true` if `cmd` is a wireless-extensions ioctl handled here.
pub fn is_wext_ioctl(cmd: u32) -> bool {
    matches!(
        cmd,
        SIOCSIWCOMMIT | SIOCSIWFREQ | SIOCSIWMODE | SIOCSIWESSID | SIOCSIWENCODEEXT
    )
}

/// Handles a wireless-extensions `ioctl`. Setters stage config; `SIOCSIWCOMMIT`
/// applies it. Returns `Ok(0)` on success.
pub fn handle(cmd: u32, arg: usize) -> AxResult<usize> {
    let ifname = read_ifname(arg)?;

    match cmd {
        SIOCSIWMODE => {
            let data = read_iwreq_data(arg)?;
            let mode = u32::from_ne_bytes([data[0], data[1], data[2], data[3]]);
            let staged = match mode {
                IW_MODE_INFRA => StagedMode::Station,
                IW_MODE_MASTER => StagedMode::AccessPoint,
                _ => return Err(AxError::InvalidInput),
            };
            with_pending(&ifname, |p| p.mode = Some(staged));
        }
        SIOCSIWESSID => {
            let ssid = read_iw_point(arg, IW_ESSID_MAX_SIZE)?;
            with_pending(&ifname, |p| p.ssid = Some(ssid));
        }
        SIOCSIWENCODEEXT => {
            // We only need the passphrase bytes. `struct iw_encode_ext` carries
            // the key at offset 24 with a preceding `u16 key_len` at offset 20;
            // but userspace tools also place the key via the iw_point buffer.
            // Accept the iw_point buffer as the raw passphrase for simplicity.
            let key = read_iw_point(arg, MAX_PASSPHRASE)?;
            let pass = String::from_utf8(key).map_err(|_| AxError::InvalidInput)?;
            with_pending(&ifname, |p| p.passphrase = Some(pass));
        }
        SIOCSIWFREQ => {
            // Interpret the first u32 of iwreq_data as a channel number (1..=14).
            let data = read_iwreq_data(arg)?;
            let chan = u32::from_ne_bytes([data[0], data[1], data[2], data[3]]);
            if chan == 0 || chan > 14 {
                return Err(AxError::InvalidInput);
            }
            with_pending(&ifname, |p| p.channel = Some(chan as u8));
        }
        SIOCSIWCOMMIT => return commit(&ifname),
        _ => return Err(AxError::Unsupported),
    }
    Ok(0)
}

/// Applies the staged config for `ifname` atomically via the network stack.
fn commit(ifname: &str) -> AxResult<usize> {
    let pending = take_pending(ifname).ok_or(AxError::InvalidInput)?;
    let mode = pending.mode.ok_or(AxError::InvalidInput)?;

    match mode {
        StagedMode::Station => {
            let ssid = pending.ssid.ok_or(AxError::InvalidInput)?;
            let ssid = core::str::from_utf8(&ssid).map_err(|_| AxError::InvalidInput)?;
            let password = pending.passphrase.unwrap_or_default();
            ax_net::reconfigure_wifi(ifname, ax_net::WifiMode::Station {
                ssid,
                password: &password,
            })?;
        }
        StagedMode::AccessPoint => {
            let ssid = pending.ssid.ok_or(AxError::InvalidInput)?;
            let channel = pending.channel.unwrap_or(AP_CHANNEL_DEFAULT);
            ax_net::reconfigure_wifi(ifname, ax_net::WifiMode::AccessPoint {
                ssid: &ssid,
                channel,
                ip: AP_SERVER_IP,
                prefix_len: AP_PREFIX_LEN,
                dhcp_client_ip: Some(AP_CLIENT_IP),
            })?;
        }
    }

    Ok(0)
}

/// Silences unused-write-helper warnings if a setter that echoes data back is
/// added later. Currently all WE setters here only stage, so no write-back.
#[allow(dead_code)]
fn _write_iwreq_data(arg: usize, data: &[u8]) -> AxResult<()> {
    Ok(vm_write_slice((arg + IWREQ_DATA_OFFSET) as *mut u8, data)?)
}

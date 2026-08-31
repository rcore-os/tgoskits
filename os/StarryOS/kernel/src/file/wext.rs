//! Linux wireless-extensions (WE) `ioctl` support for socket fds.
//!
//! Implements the small subset of the wireless extensions a userspace program
//! (or the on-device HTTP control server) needs to switch a Wi-Fi interface
//! between Station and SoftAP at runtime:
//!
//! - `SIOCSIWMODE`    — stage the target mode (Managed/STA vs Master/AP).
//! - `SIOCSIWESSID`   — stage the SSID.
//! - `SIOCSIWENCODEEXT` — stage a Linux `IW_ENCODE_ALG_PMK` key (STA).
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

use starry_vm::{vm_read_slice, vm_write_slice};

use crate::{StarryError, StarryResult, sync::IrqMutex as Mutex};

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

/// Linux `struct iw_encode_ext` fixed header and supported PMK payload sizes.
const IW_ENCODE_EXT_HEADER_SIZE: usize = 40;
const IW_ENCODE_TOKEN_MAX: usize = 64;
const IW_ENCODE_ALG_PMK: u16 = 4;
const WPA2_PMK_SIZE: usize = 32;

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
struct Pending {
    ifname: String,
    mode: Option<StagedMode>,
    ssid: Option<Vec<u8>>,
    pmk: Option<ax_net::Wpa2Pmk>,
    channel: Option<u8>,
}

impl Pending {
    fn new(ifname: String) -> Self {
        Self {
            ifname,
            mode: None,
            ssid: None,
            pmk: None,
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

fn read_user_array<const N: usize>(ptr: *const u8) -> StarryResult<[u8; N]> {
    let mut buf = [MaybeUninit::<u8>::uninit(); N];
    vm_read_slice(ptr, &mut buf)?;
    Ok(buf.map(|v| unsafe { v.assume_init() }))
}

fn read_ifname(arg: usize) -> StarryResult<String> {
    let buf = read_user_array::<IWREQ_NAME_LEN>(arg as *const u8)?;
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..end].to_vec()).map_err(|_| StarryError::InvalidInput)
}

/// Reads the 16-byte `union iwreq_data` payload following the name.
fn read_iwreq_data(arg: usize) -> StarryResult<[u8; 16]> {
    read_user_array::<16>((arg + IWREQ_DATA_OFFSET) as *const u8)
}

/// Reads a length-prefixed userspace buffer described by an `iw_point`
/// (`{ void* pointer; u16 length; u16 flags; }`) embedded in `iwreq_data`.
fn read_iw_point(arg: usize, max: usize) -> StarryResult<(Vec<u8>, u16)> {
    let data = read_iwreq_data(arg)?;
    let ptr = usize::from_ne_bytes(
        data[..core::mem::size_of::<usize>()]
            .try_into()
            .map_err(|_| StarryError::InvalidInput)?,
    );
    let length_offset = core::mem::size_of::<usize>();
    let len = u16::from_ne_bytes([data[length_offset], data[length_offset + 1]]) as usize;
    let flags = u16::from_ne_bytes([data[length_offset + 2], data[length_offset + 3]]);
    if ptr == 0 || len == 0 {
        return Ok((Vec::new(), flags));
    }
    if len > max {
        return Err(StarryError::ArgumentListTooLong);
    }
    let mut buf = alloc::vec![MaybeUninit::<u8>::uninit(); len];
    vm_read_slice(ptr as *const u8, &mut buf)?;
    Ok((
        buf.into_iter()
            .map(|v| unsafe { v.assume_init() })
            .collect(),
        flags,
    ))
}

fn parse_iw_frequency(data: &[u8; 16]) -> StarryResult<u8> {
    let mantissa = i32::from_ne_bytes([data[0], data[1], data[2], data[3]]);
    let exponent = i16::from_ne_bytes([data[4], data[5]]);
    if exponent == 0 && (1..=14).contains(&mantissa) {
        return Ok(mantissa as u8);
    }
    let mut frequency_hz = i64::from(mantissa);
    for _ in 0..exponent {
        frequency_hz = frequency_hz
            .checked_mul(10)
            .ok_or(StarryError::InvalidInput)?;
    }
    let frequency_mhz = frequency_hz / 1_000_000;
    match frequency_mhz {
        2_484 => Ok(14),
        2_412..=2_472 if (frequency_mhz - 2_407) % 5 == 0 => {
            Ok(((frequency_mhz - 2_407) / 5) as u8)
        }
        _ => Err(StarryError::InvalidInput),
    }
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
pub fn handle(cmd: u32, arg: usize) -> StarryResult<usize> {
    let ifname = read_ifname(arg)?;

    match cmd {
        SIOCSIWMODE => {
            let data = read_iwreq_data(arg)?;
            let mode = u32::from_ne_bytes([data[0], data[1], data[2], data[3]]);
            let staged = match mode {
                IW_MODE_INFRA => StagedMode::Station,
                IW_MODE_MASTER => StagedMode::AccessPoint,
                _ => return Err(StarryError::InvalidInput),
            };
            with_pending(&ifname, |p| p.mode = Some(staged));
        }
        SIOCSIWESSID => {
            let (mut ssid, flags) = read_iw_point(arg, IW_ESSID_MAX_SIZE + 1)?;
            if ssid.len() == IW_ESSID_MAX_SIZE + 1 {
                if ssid.last() != Some(&0) {
                    return Err(StarryError::ArgumentListTooLong);
                }
                ssid.pop();
            }
            with_pending(&ifname, |pending| {
                pending.ssid = (flags != 0).then_some(ssid);
                if flags == 0 {
                    pending.pmk = None;
                }
            });
        }
        SIOCSIWENCODEEXT => {
            let (encoded, _) =
                read_iw_point(arg, IW_ENCODE_EXT_HEADER_SIZE + IW_ENCODE_TOKEN_MAX)?;
            let pmk = parse_pmk_encode_ext(&encoded)?;
            with_pending(&ifname, |p| p.pmk = Some(pmk));
        }
        SIOCSIWFREQ => {
            let data = read_iwreq_data(arg)?;
            let channel = parse_iw_frequency(&data)?;
            with_pending(&ifname, |p| p.channel = Some(channel));
        }
        SIOCSIWCOMMIT => return commit(&ifname),
        _ => return Err(StarryError::Unsupported),
    }
    Ok(0)
}

/// Applies the staged config for `ifname` atomically via the network stack.
fn commit(ifname: &str) -> StarryResult<usize> {
    let pending = take_pending(ifname).ok_or(StarryError::InvalidInput)?;
    let mode = pending.mode.ok_or(StarryError::InvalidInput)?;

    match mode {
        StagedMode::Station => {
            let ssid = pending.ssid.ok_or(StarryError::InvalidInput)?;
            let ssid = String::from_utf8(ssid).map_err(|_| StarryError::InvalidInput)?;
            let transaction = match pending.pmk {
                Some(pmk) => ax_net::WifiTransaction::connect_wpa2_pmk(ssid, pmk),
                None => ax_net::WifiTransaction::connect_open(ssid),
            };
            info!("[wifi] {ifname}: applying staged station configuration");
            if let Err(error) = ax_net::reconfigure_wifi(ifname, transaction) {
                error!("[wifi] {ifname}: station configuration failed: {error:?}");
                return Err(error.into());
            }
        }
        StagedMode::AccessPoint => {
            let ssid = pending.ssid.ok_or(StarryError::InvalidInput)?;
            let channel = pending.channel.unwrap_or(AP_CHANNEL_DEFAULT);
            ax_net::reconfigure_wifi(
                ifname,
                ax_net::WifiTransaction::open_access_point(
                    ssid,
                    channel,
                    ax_net::WifiLinkPolicy {
                        ip: AP_SERVER_IP,
                        prefix_len: AP_PREFIX_LEN,
                        dhcp_server_client_ip: Some(AP_CLIENT_IP),
                    },
                ),
            )?;
        }
    }

    Ok(0)
}

/// Parses the native-endian Linux UAPI layout:
/// `iw_point.pointer -> struct iw_encode_ext { ...; u16 alg; u16 key_len; u8 key[]; }`.
fn parse_pmk_encode_ext(encoded: &[u8]) -> StarryResult<ax_net::Wpa2Pmk> {
    if encoded.len() < IW_ENCODE_EXT_HEADER_SIZE {
        return Err(StarryError::InvalidInput);
    }
    let algorithm = u16::from_ne_bytes([encoded[36], encoded[37]]);
    let key_length = u16::from_ne_bytes([encoded[38], encoded[39]]) as usize;
    if algorithm != IW_ENCODE_ALG_PMK {
        return Err(StarryError::OperationNotSupported);
    }
    if key_length != WPA2_PMK_SIZE
        || encoded.len() != IW_ENCODE_EXT_HEADER_SIZE + key_length
    {
        return Err(StarryError::InvalidInput);
    }
    let key: [u8; WPA2_PMK_SIZE] = encoded[IW_ENCODE_EXT_HEADER_SIZE..]
        .try_into()
        .map_err(|_| StarryError::InvalidInput)?;
    Ok(ax_net::Wpa2Pmk::new(key))
}

/// Silences unused-write-helper warnings if a setter that echoes data back is
/// added later. Currently all WE setters here only stage, so no write-back.
#[allow(dead_code)]
fn _write_iwreq_data(arg: usize, data: &[u8]) -> StarryResult<()> {
    Ok(vm_write_slice((arg + IWREQ_DATA_OFFSET) as *mut u8, data)?)
}

#[cfg(all(test, not(axtest)))]
fn is_wext_ioctl_validation_rules_hold_for_test() -> bool {
    // is_wext_ioctl: returns true only for the 5 handled WE ioctl commands.
    let valid_cmds = [
        SIOCSIWCOMMIT,
        SIOCSIWFREQ,
        SIOCSIWMODE,
        SIOCSIWESSID,
        SIOCSIWENCODEEXT,
    ];
    let all_valid = valid_cmds.iter().all(|&cmd| is_wext_ioctl(cmd));

    // Non-WE commands should return false.
    let invalid = !is_wext_ioctl(0)
        && !is_wext_ioctl(u32::MAX)
        && !is_wext_ioctl(SIOCSIWCOMMIT + 1)
        && !is_wext_ioctl(SIOCSIWCOMMIT - 1);

    all_valid && invalid
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn is_wext_ioctl_validation_rules_hold() {
        assert!(super::is_wext_ioctl_validation_rules_hold_for_test());
    }

    #[test]
    fn encode_ext_uses_the_linux_pmk_layout_and_rejects_raw_passwords() {
        let mut encoded = alloc::vec![0; super::IW_ENCODE_EXT_HEADER_SIZE + 32];
        encoded[36..38].copy_from_slice(&super::IW_ENCODE_ALG_PMK.to_ne_bytes());
        encoded[38..40].copy_from_slice(&32u16.to_ne_bytes());
        encoded[40..].fill(0x5a);

        let pmk = super::parse_pmk_encode_ext(&encoded).unwrap();
        assert_eq!(pmk.bytes(), &[0x5a; 32]);
        assert!(super::parse_pmk_encode_ext(b"raw-passphrase").is_err());

        encoded[36..38].copy_from_slice(&3u16.to_ne_bytes());
        assert!(matches!(
            super::parse_pmk_encode_ext(&encoded),
            Err(crate::StarryError::OperationNotSupported)
        ));
    }

    #[test]
    fn frequency_parser_accepts_linux_channel_and_frequency_encodings() {
        let mut channel = [0; 16];
        channel[..4].copy_from_slice(&6i32.to_ne_bytes());
        assert_eq!(super::parse_iw_frequency(&channel).unwrap(), 6);

        let mut frequency = [0; 16];
        frequency[..4].copy_from_slice(&2_437i32.to_ne_bytes());
        frequency[4..6].copy_from_slice(&6i16.to_ne_bytes());
        assert_eq!(super::parse_iw_frequency(&frequency).unwrap(), 6);

        frequency[4..6].copy_from_slice(&(-1i16).to_ne_bytes());
        assert!(super::parse_iw_frequency(&frequency).is_err());
    }
}

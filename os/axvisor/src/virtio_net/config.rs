//! Configuration parsing and validation for the AxVisor virtio-net glue.
//!
//! Each virtio-net MMIO device is described by one `EmulatedDeviceConfig` row.
//! Per the integration contract (plan section 0), `cfg_list` carries exactly the
//! 6 guest MAC octets; the deterministic virtual peer's MAC, IPv4 address and
//! UDP echo port are fixed constants shared with the guest smoke application, so
//! the smoke flow stays reproducible without extra config plumbing.

use axdevice::DeviceManagerError;
use axvm_types::EmulatedDeviceConfig;

/// Minimum MMIO window length: the virtio-mmio transport register block
/// (`0x100`) plus the net config space (MAC + status + max_virtqueue_pairs +
/// mtu = 12 bytes).
pub const MIN_MMIO_LENGTH: usize = 0x100 + 12;

/// SPI interrupt ids start at 32; ids 0..31 are SGIs/PPIs (timer, SGIs).
pub const MIN_SPI_IRQ_ID: usize = 32;

/// Page size used for MMIO base alignment validation.
pub const PAGE_SIZE: usize = 0x1000;

/// Maximum number of inbound frames buffered per guest port. The queue must
/// absorb concurrent uplink responses and local-switch TCP bursts while the
/// shared delivery CPU services another guest.
pub const RX_QUEUE_CAPACITY: usize = 1024;
/// Maximum number of guest TX frames buffered until the uplink worker can
/// classify and forward them.
pub const TX_QUEUE_CAPACITY: usize = 1024;

/// Fixed MAC of the deterministic virtual peer (the echo node).
///
/// Mirrored by the guest smoke application.
pub const PEER_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x00, 0x00, 0x01];

/// Fixed IPv4 address of the deterministic virtual peer.
pub const PEER_IPV4: [u8; 4] = [10, 0, 0, 1];

/// Fixed IPv4 address the guest smoke application assigns to its interface.
pub const GUEST_IPV4: [u8; 4] = [10, 0, 0, 2];

/// UDP destination port the echo peer answers on.
pub const ECHO_UDP_PORT: u16 = 4433;

/// A validated virtio-net device specification parsed from a config row.
#[derive(Clone, Debug)]
pub struct VirtioNetDeviceSpec {
    pub name: alloc::string::String,
    pub base_gpa: usize,
    pub length: usize,
    pub irq_id: usize,
    pub mac: [u8; 6],
    pub backend: BackendSpec,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendSpec {
    DeterministicPeer,
    RawUplink { mac: [u8; 6] },
}

impl VirtioNetDeviceSpec {
    /// Parses and validates a virtio-net device from its emulated-device row.
    ///
    /// # Errors
    ///
    /// Returns [`DeviceManagerError::InvalidConfig`] if `cfg_list` does not
    /// carry exactly 6 MAC octets each `<= 255`, if the MMIO window is not
    /// page-aligned or too small, or if the IRQ is not an SPI (`>= 32`).
    pub fn from_config(config: &EmulatedDeviceConfig) -> Result<Self, DeviceManagerError> {
        let (mac, backend) = parse_backend(&config.cfg_list, &config.name)?;
        validate_mmio_window(config.base_gpa, config.length, &config.name)?;
        validate_irq(config.irq_id, &config.name)?;
        Ok(Self {
            name: config.name.clone(),
            base_gpa: config.base_gpa,
            length: config.length,
            irq_id: config.irq_id,
            mac,
            backend,
        })
    }
}

fn parse_backend(
    cfg_list: &[usize],
    name: &str,
) -> Result<([u8; 6], BackendSpec), DeviceManagerError> {
    match cfg_list.len() {
        6 => Ok((
            parse_mac_octets(cfg_list, name)?,
            BackendSpec::DeterministicPeer,
        )),
        13 => {
            let guest_mac = parse_mac_octets(&cfg_list[..6], name)?;
            let mode = cfg_list[6];
            if mode != 1 {
                return Err(invalid_config(
                    name,
                    alloc::format!("unsupported backend mode {mode}"),
                ));
            }
            let uplink_mac = parse_mac_octets(&cfg_list[7..], name)?;
            Ok((guest_mac, BackendSpec::RawUplink { mac: uplink_mac }))
        }
        length => Err(invalid_config(
            name,
            alloc::format!(
                "cfg_list must contain guest MAC (6 values) or guest MAC + mode + uplink MAC (13 values), got {length}"
            ),
        )),
    }
}

fn parse_mac_octets(cfg_list: &[usize], name: &str) -> Result<[u8; 6], DeviceManagerError> {
    if cfg_list.len() != 6 {
        return Err(invalid_config(
            name,
            alloc::format!(
                "cfg_list must carry exactly 6 MAC octets, got {}",
                cfg_list.len()
            ),
        ));
    }
    let mut mac = [0u8; 6];
    for (index, octet) in cfg_list.iter().enumerate() {
        if *octet > u8::MAX as usize {
            return Err(invalid_config(
                name,
                alloc::format!("cfg_list MAC octet {index} = {octet} exceeds 255"),
            ));
        }
        mac[index] = *octet as u8;
    }
    Ok(mac)
}

fn validate_mmio_window(base: usize, length: usize, name: &str) -> Result<(), DeviceManagerError> {
    if base % PAGE_SIZE != 0 {
        return Err(invalid_config(
            name,
            alloc::format!("MMIO base {base:#x} is not page-aligned ({PAGE_SIZE:#x})"),
        ));
    }
    if length < MIN_MMIO_LENGTH {
        return Err(invalid_config(
            name,
            alloc::format!(
                "MMIO length {length:#x} is smaller than the minimum {MIN_MMIO_LENGTH:#x}"
            ),
        ));
    }
    Ok(())
}

fn validate_irq(irq_id: usize, name: &str) -> Result<(), DeviceManagerError> {
    if irq_id < MIN_SPI_IRQ_ID {
        return Err(invalid_config(
            name,
            alloc::format!("irq_id {irq_id} must be an SPI (>= {MIN_SPI_IRQ_ID})"),
        ));
    }
    Ok(())
}

fn invalid_config(name: &str, detail: alloc::string::String) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation: "virtio-net config",
        detail: alloc::format!("[virtio-net {name}] {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_deterministic_and_raw_uplink_backend_shapes() {
        let guest_mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let uplink_mac = [0x52, 0x54, 0, 0xaa, 0xbb, 1];

        let (mac, backend) = parse_backend(&guest_mac.map(usize::from), "net").unwrap();
        assert_eq!(mac, guest_mac);
        assert_eq!(backend, BackendSpec::DeterministicPeer);

        let mut raw = guest_mac.map(usize::from).to_vec();
        raw.push(1);
        raw.extend(uplink_mac.map(usize::from));
        let (mac, backend) = parse_backend(&raw, "net").unwrap();
        assert_eq!(mac, guest_mac);
        assert_eq!(backend, BackendSpec::RawUplink { mac: uplink_mac });
    }

    #[test]
    fn rejects_unknown_backend_mode_and_invalid_mac_octet() {
        let mut raw = alloc::vec![0; 13];
        raw[6] = 2;
        assert!(parse_backend(&raw, "net").is_err());

        raw[6] = 1;
        raw[7] = 256;
        assert!(parse_backend(&raw, "net").is_err());
    }
}

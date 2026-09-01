//! Firmware-assigned link identities and their validity invariants.

use crate::device::AicError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(super) struct InterfaceIndex(u8);

impl InterfaceIndex {
    pub(super) fn from_firmware(value: u8) -> Result<Self, AicError> {
        (value != u8::MAX)
            .then_some(Self(value))
            .ok_or(AicError::MalformedResponse)
    }

    pub(super) const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(super) struct StationIndex(u8);

impl StationIndex {
    pub(super) fn from_firmware(value: u8) -> Result<Self, AicError> {
        (value != u8::MAX)
            .then_some(Self(value))
            .ok_or(AicError::MalformedResponse)
    }

    pub(super) const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LinkState {
    mac_address: Option<[u8; 6]>,
    interface: Option<InterfaceIndex>,
    peer: Option<(StationIndex, [u8; 6])>,
    control_port_open: bool,
}

impl LinkState {
    pub(super) const fn new() -> Self {
        Self {
            mac_address: None,
            interface: None,
            peer: None,
            control_port_open: false,
        }
    }

    pub(super) fn install_mac(&mut self, mac: [u8; 6]) -> Result<(), AicError> {
        let multicast = mac[0] & 1 != 0;
        if mac == [0; 6] || multicast {
            return Err(AicError::InvalidMacAddress);
        }
        self.mac_address = Some(mac);
        Ok(())
    }

    pub(super) const fn mac_address(&self) -> Option<[u8; 6]> {
        self.mac_address
    }

    pub(super) fn install_interface(&mut self, index: u8) -> Result<(), AicError> {
        self.interface = Some(InterfaceIndex::from_firmware(index)?);
        self.clear_peer();
        Ok(())
    }

    pub(super) fn install_peer(
        &mut self,
        interface: u8,
        station: u8,
        bssid: [u8; 6],
    ) -> Result<(), AicError> {
        let expected = self.interface.ok_or(AicError::MalformedResponse)?;
        let actual = InterfaceIndex::from_firmware(interface)?;
        if actual != expected || bssid == [0; 6] || bssid[0] & 1 != 0 {
            return Err(AicError::MalformedResponse);
        }
        self.peer = Some((StationIndex::from_firmware(station)?, bssid));
        self.control_port_open = false;
        Ok(())
    }

    pub(super) const fn tx_indices(&self) -> Option<(u8, u8)> {
        match (self.interface, self.peer) {
            (Some(interface), Some((station, _))) => Some((interface.get(), station.get())),
            _ => None,
        }
    }

    pub(super) const fn interface_index(&self) -> Option<u8> {
        match self.interface {
            Some(index) => Some(index.get()),
            None => None,
        }
    }

    pub(super) const fn peer(&self) -> Option<(u8, [u8; 6])> {
        match self.peer {
            Some((index, bssid)) => Some((index.get(), bssid)),
            None => None,
        }
    }

    pub(super) fn open_control_port(&mut self) -> Result<(), AicError> {
        if self.peer.is_none() {
            return Err(AicError::MalformedResponse);
        }
        self.control_port_open = true;
        Ok(())
    }

    pub(super) fn clear_peer(&mut self) {
        self.peer = None;
        self.control_port_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_and_multicast_mac_addresses() {
        let mut link = LinkState::new();
        assert_eq!(link.install_mac([0; 6]), Err(AicError::InvalidMacAddress));
        assert_eq!(
            link.install_mac([1, 0, 0, 0, 0, 1]),
            Err(AicError::InvalidMacAddress)
        );
    }
}

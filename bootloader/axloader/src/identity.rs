extern crate alloc;

use alloc::string::String;

use uefi::{
    boot::{self, OpenProtocolAttributes, OpenProtocolParams},
    proto::network::ip4config2::Ip4Config2,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityError {
    NoIp4Config2,
    OpenFailed,
    InterfaceInfoFailed,
    UnsupportedMacSize,
}

pub fn mac_address_string() -> Result<String, IdentityError> {
    let handles = boot::find_handles::<Ip4Config2>().map_err(|_| IdentityError::NoIp4Config2)?;
    for handle in handles.iter().copied() {
        let mut protocol = match unsafe {
            boot::open_protocol::<Ip4Config2>(
                OpenProtocolParams {
                    handle,
                    agent: boot::image_handle(),
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        } {
            Ok(protocol) => protocol,
            Err(_) => continue,
        };
        let info = protocol
            .get_interface_info()
            .map_err(|_| IdentityError::InterfaceInfoFailed)?;
        if info.hw_addr_size < 6 {
            return Err(IdentityError::UnsupportedMacSize);
        }
        return Ok(format_mac(&info.hw_addr.octets()[..6]));
    }

    Err(IdentityError::OpenFailed)
}

fn format_mac(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(17);
    for (index, byte) in bytes.iter().take(6).enumerate() {
        if index != 0 {
            output.push(':');
        }
        push_hex_byte(&mut output, *byte);
    }
    output
}

fn push_hex_byte(output: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.push(char::from(HEX[(byte >> 4) as usize]));
    output.push(char::from(HEX[(byte & 0x0f) as usize]));
}

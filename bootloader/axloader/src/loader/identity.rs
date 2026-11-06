//! One immutable identity per firmware boot, established before discovery.

use alloc::string::String;
use core::fmt::Write;

use httpboot_protocol::MacAddress;
use sha2::{Digest, Sha256};
use uefi::{Status, boot, proto::rng::Rng};

pub(super) struct BootIdentity {
    pub epoch: String,
    pub serial_id: String,
}

impl BootIdentity {
    pub fn establish(mac_address: MacAddress) -> Result<Self, Status> {
        let mut seed = [0_u8; 32];
        boot::get_handle_for_protocol::<Rng>()
            .and_then(boot::open_protocol_exclusive::<Rng>)
            .and_then(|mut rng| rng.get_rng(None, &mut seed))
            .map_err(|error| error.status())?;
        Ok(Self {
            epoch: derive(&seed, b"boot-epoch", mac_address),
            serial_id: derive(&seed, b"serial-id", mac_address),
        })
    }
}

fn derive(seed: &[u8; 32], domain: &[u8], mac_address: MacAddress) -> String {
    let mut hash = Sha256::new();
    hash.update(b"axloader-boot-identity-v1\0");
    hash.update(domain);
    hash.update([0]);
    hash.update(mac_address.octets());
    hash.update(seed);
    let digest = hash.finalize();
    let mut value = String::with_capacity(32);
    for byte in &digest[..16] {
        write!(value, "{byte:02x}").expect("write to String");
    }
    value
}

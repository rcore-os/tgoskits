//! One immutable identity per firmware boot, established before discovery.

use alloc::string::String;
use core::fmt::Write;

use httpboot_protocol::MacAddress;
use sha2::{Digest, Sha256};
use uefi::{Status, boot, proto::rng::Rng};

use super::arch;

pub(super) struct BootIdentity {
    pub epoch: String,
    pub serial_id: String,
}

impl BootIdentity {
    pub fn establish(mac_address: MacAddress) -> Result<Self, Status> {
        let seed = random_seed().or_else(|error| {
            if arch::ALLOW_IDENTITY_COUNTER_FALLBACK {
                monotonic_seed(mac_address)
            } else {
                Err(error)
            }
        })?;
        Ok(Self {
            epoch: derive(&seed, b"boot-epoch", mac_address),
            serial_id: derive(&seed, b"serial-id", mac_address),
        })
    }
}

fn random_seed() -> Result<[u8; 32], Status> {
    let mut seed = [0_u8; 32];
    boot::get_handle_for_protocol::<Rng>()
        .and_then(boot::open_protocol_exclusive::<Rng>)
        .and_then(|mut rng| rng.get_rng(None, &mut seed))
        .map_err(|error| error.status())?;
    Ok(seed)
}

fn monotonic_seed(mac_address: MacAddress) -> Result<[u8; 32], Status> {
    let mut count = 0_u64;
    let table = uefi::table::system_table_raw().ok_or(Status::NOT_READY)?;
    // SAFETY: This runs at APPLICATION before ExitBootServices with the live
    // SystemTable. The call only initializes the aligned local counter and
    // retains no pointer. No Rust reference to the service table is created.
    let status = unsafe {
        let services = (*table.as_ptr()).boot_services;
        if services.is_null() {
            return Err(Status::NOT_READY);
        }
        ((*services).get_next_monotonic_count)(&mut count)
    };
    if status != Status::SUCCESS {
        return Err(status);
    }
    let clock = uefi::runtime::get_time().ok();
    let mut hash = Sha256::new();
    hash.update(alloc::format!(":{count}:{clock:?}:{mac_address:?}"));
    Ok(hash.finalize().into())
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

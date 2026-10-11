pub mod boot_server;
pub mod console;
pub mod direct;
pub mod elf_loader;
pub mod entry;
pub mod network;
pub mod payload;
mod serial;
pub mod smbios;

use alloc::{format, string::String};
use core::time::Duration;

use sha2::{Digest, Sha256};
use uefi::{Status, boot, prelude::*, proto::rng::Rng};

use crate::logln;

#[entry]
fn efi_main() -> Status {
    uefi::helpers::init().expect("failed to initialize UEFI helpers");
    let mut ota = axloader::ota::OtaController::open();
    let mut interface = None;
    loop {
        if interface.is_none() {
            interface = network::NetworkInterface::select().ok();
        }
        let Some(nic) = interface else {
            logln!("loader_tcp4_unavailable: no matching SNP/IP4/UDP4/TCP4 controller");
            boot::stall(Duration::from_secs(2));
            continue;
        };
        let identity = new_boot_identity(nic, b"boot-epoch").and_then(|epoch| {
            new_boot_identity(nic, b"serial-id").map(|serial_id| (epoch, serial_id))
        });
        let (epoch, serial_id) = match identity {
            Ok(identity) => identity,
            Err(error) => {
                logln!("loader_identity_unavailable: {error:?}");
                boot::stall(Duration::from_secs(2));
                continue;
            }
        };
        let mut beacon = serial::SerialBeacon::new(serial_id);
        let mut server = boot_server::BootServer::new(
            epoch,
            nic.mac_address,
            nic.current_mac_address,
            smbios::hardware_info(),
            beacon.state.clone(),
        );
        let mut announcer = match network::Announcer::new(nic, server.epoch()) {
            Ok(value) => Some(value),
            Err(error) => {
                logln!("loader_broadcast_unavailable: {error:?}");
                None
            }
        };
        let mut listener = match direct::Listener::open(nic.handle()) {
            Ok(value) => value,
            Err(error) => {
                logln!("loader_tcp4_listen_failed: {error:?}");
                boot::stall(Duration::from_secs(2));
                continue;
            }
        };
        logln!(
            "loader_device_listening: port=2999 epoch={}",
            server.epoch()
        );
        let mut ticks = 200_u16;
        loop {
            let mut progress = || {
                beacon.progress();
                ticks = ticks.saturating_add(1);
                if ticks >= 200 {
                    ticks = 0;
                    if let Some(announcer) = announcer.as_mut()
                        && let Err(error) = announcer.broadcast(&beacon.state.borrow())
                    {
                        logln!("loader_broadcast_error: {error:?}");
                    }
                }
            };
            progress();
            match listener.poll(&mut ota, &mut server, &mut progress) {
                direct::Action::None => boot::stall(Duration::from_millis(10)),
                direct::Action::Reset => {
                    logln!("ota_staged_reboot");
                    drop(listener);
                    drop(announcer);
                    drop(beacon);
                    uefi::runtime::reset(uefi::runtime::ResetType::COLD, Status::SUCCESS, None);
                }
                direct::Action::Boot(execution) => {
                    let boot_server::BootExecution {
                        elf,
                        payload,
                        load_options,
                    } = *execution;
                    logln!(
                        "elf_loaded: load={:#x} end={:#x} entry={:#x}",
                        elf.load_addr,
                        elf.load_end,
                        elf.entry_point
                    );
                    logln!("ready_to_handoff");
                    drop(listener);
                    drop(announcer);
                    drop(beacon);
                    let result = entry::jump_to_uefi_entry(elf.entry_point, load_options);
                    drop(payload);
                    boot_server::free_loaded_elf(&elf);
                    logln!("jump_error: {result:?}");
                    return Status::LOAD_ERROR;
                }
            }
        }
    }
}

fn new_boot_identity(nic: network::NetworkInterface, domain: &[u8]) -> uefi::Result<String> {
    let mut bytes = [0_u8; 16];
    if let Ok(handle) = boot::get_handle_for_protocol::<Rng>()
        && let Ok(mut rng) = boot::open_protocol_exclusive::<Rng>(handle)
        && rng.get_rng(None, &mut bytes).is_ok()
    {
        return Ok(hex(&bytes));
    }
    // These identities only distinguish boots and are not authentication secrets.
    // Domain separation keeps the fallback epoch and serial ID independent even
    // when both are derived from the same monotonic counter and clock sample.
    let mut count = 0_u64;
    let table = uefi::table::system_table_raw().ok_or(Status::NOT_READY)?;
    // SAFETY: this runs at APPLICATION before ExitBootServices, with the initialized
    // firmware SystemTable. The call initializes the aligned local u64 and does not
    // retain its address. No mutable Rust reference to the service table is created.
    let status = unsafe {
        let services = (*table.as_ptr()).boot_services;
        if services.is_null() {
            return Err(Status::NOT_READY.into());
        }
        ((*services).get_next_monotonic_count)(&mut count)
    };
    if status != Status::SUCCESS {
        return Err(status.into());
    }
    let clock = uefi::runtime::get_time().ok();
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(format!(":{count}:{clock:?}:{:?}", nic.mac_address));
    bytes.copy_from_slice(&hash.finalize()[..16]);
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write;
    let mut value = String::new();
    for byte in bytes {
        write!(value, "{byte:02x}").expect("write to String");
    }
    value
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    logln!("panic: {info}");
    loop {
        boot::stall(Duration::from_secs(1));
    }
}

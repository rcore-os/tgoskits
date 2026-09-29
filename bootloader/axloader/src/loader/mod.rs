pub mod boot_server;
pub mod console;
pub mod direct;
pub mod elf_loader;
pub mod entry;
pub mod network;
pub mod payload;
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
        let epoch = new_boot_epoch(nic);
        let mut server = boot_server::BootServer::new(
            epoch,
            nic.mac_address,
            nic.current_mac_address,
            smbios::hardware_info(),
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
                ticks = ticks.saturating_add(1);
                if ticks >= 200 {
                    ticks = 0;
                    if let Some(announcer) = announcer.as_mut()
                        && let Err(error) = announcer.broadcast()
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
                    uefi::runtime::reset(uefi::runtime::ResetType::COLD, Status::SUCCESS, None);
                }
                direct::Action::Boot(execution) => {
                    let boot_server::BootExecution {
                        elf,
                        payload,
                        load_options,
                    } = execution;
                    logln!(
                        "elf_loaded: load={:#x} end={:#x} entry={:#x}",
                        elf.load_addr,
                        elf.load_end,
                        elf.entry_point
                    );
                    logln!("ready_to_handoff");
                    drop(listener);
                    drop(announcer);
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

fn new_boot_epoch(nic: network::NetworkInterface) -> String {
    let mut bytes = [0_u8; 16];
    if let Ok(handle) = boot::get_handle_for_protocol::<Rng>()
        && let Ok(mut rng) = boot::open_protocol_exclusive::<Rng>(handle)
        && rng.get_rng(None, &mut bytes).is_ok()
    {
        return hex(&bytes);
    }
    // The epoch only distinguishes boots; it is not an authentication secret.
    let clock = uefi::runtime::get_time().ok();
    let mut hash = Sha256::new();
    hash.update(format!("{clock:?}:{:?}:{:p}", nic.mac_address, &clock));
    bytes.copy_from_slice(&hash.finalize()[..16]);
    hex(&bytes)
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

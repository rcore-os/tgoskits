pub mod boot_server;
mod cache;
pub mod console;
pub mod direct;
pub mod elf_loader;
pub mod network;
pub mod serial;
pub mod smbios;

use alloc::{format, string::String};
use core::time::Duration;

use sha2::{Digest, Sha256};
use uefi::{Status, boot, prelude::*, proto::rng::Rng};

use crate::logln;

const MAX_BOOTSTRAP_DIAGNOSTICS: usize = 3;
const WATCHDOG_APPLICATION_CODE: u64 = 0x0001_0000;

#[entry]
fn efi_main() -> Status {
    uefi::helpers::init().expect("failed to initialize UEFI helpers");
    if let Err(error) = boot::set_watchdog_timer(0, WATCHDOG_APPLICATION_CODE, None) {
        let status = error.status();
        logln!("loader_watchdog_disable_failed: {status:?}");
        return status;
    }
    let mut ota = axloader::ota::OtaController::open();
    let mut interface = None;
    let mut discovery_attempts = 0;
    let mut identity_failures = 0;
    let mut listen_failures = 0;
    loop {
        if interface.is_none() {
            interface =
                network::NetworkInterface::select(discovery_attempts < MAX_BOOTSTRAP_DIAGNOSTICS)
                    .ok();
        }
        let Some(nic) = interface else {
            if discovery_attempts < MAX_BOOTSTRAP_DIAGNOSTICS {
                logln!("loader_tcp4_unavailable: no matching SNP/IP4Config2/UDP4/TCP4 controller");
            }
            discovery_attempts = discovery_attempts.saturating_add(1);
            boot::stall(Duration::from_secs(2));
            continue;
        };
        let identity = new_boot_identity(nic, b"boot-epoch").and_then(|epoch| {
            new_boot_identity(nic, b"serial-id").map(|serial_id| (epoch, serial_id))
        });
        let (epoch, serial_id) = match identity {
            Ok(identity) => {
                identity_failures = 0;
                identity
            }
            Err(error) => {
                if identity_failures < MAX_BOOTSTRAP_DIAGNOSTICS {
                    logln!("loader_identity_unavailable: {error:?}");
                }
                identity_failures = identity_failures.saturating_add(1);
                boot::stall(Duration::from_secs(2));
                continue;
            }
        };
        let mut listener = match direct::Listener::open(nic.handle()) {
            Ok(value) => value,
            Err(error) => {
                if listen_failures < MAX_BOOTSTRAP_DIAGNOSTICS {
                    logln!(
                        "loader_tcp4_listen_failed: controller={} port=2999 status={error:?}",
                        nic.controller_index()
                    );
                }
                listen_failures = listen_failures.saturating_add(1);
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
        logln!(
            "loader_device_listening: controller={} mac={} port=2999 epoch={}",
            nic.controller_index(),
            nic.mac_address,
            server.epoch()
        );
        let mut ticks = 200_u16;
        let mut broadcast_failures = 0;
        loop {
            let mut progress = || {
                beacon.progress();
                ticks = ticks.saturating_add(1);
                if ticks >= 200 {
                    ticks = 0;
                    if let Some(announcer) = announcer.as_mut() {
                        match announcer.broadcast(&beacon.state.borrow()) {
                            Ok(()) => broadcast_failures = 0,
                            Err(error) => {
                                if broadcast_failures < MAX_BOOTSTRAP_DIAGNOSTICS {
                                    logln!("loader_broadcast_error: {error:?}");
                                }
                                broadcast_failures = broadcast_failures.saturating_add(1);
                            }
                        }
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
                    drop(server);
                    uefi::runtime::reset(uefi::runtime::ResetType::COLD, Status::SUCCESS, None);
                }
                direct::Action::Boot(execution) => {
                    let boot_server::BootExecution { elf, handoff } = *execution;
                    logln!(
                        "elf_loaded: load={:#x} end={:#x} entry={:#x}",
                        elf.load_addr,
                        elf.load_end,
                        elf.entry_point
                    );
                    logln!("ready_to_handoff");
                    drop(listener);
                    logln!("handoff_stage: listener_closed");
                    drop(announcer);
                    drop(beacon);
                    drop(server);
                    logln!("handoff_stage: resources_closed");
                    // SAFETY: ELF validation found the EFI entry in the live image;
                    // networking has been closed and the guard retains payloads.
                    logln!("handoff_stage: target_enter");
                    let result = unsafe { handoff.enter(elf.entry_point) };
                    drop(handoff);
                    drop(elf);
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

//! EFI control loop over firmware TCP4 networking.

use core::time::Duration;

use uefi::{Status, boot};

use crate::{
    loader::{arch, boot_server, direct, identity::BootIdentity, network, serial, smbios},
    logln,
};

const MAX_BOOTSTRAP_DIAGNOSTICS: usize = 3;

pub(super) fn run(mut interface: Option<network::NetworkInterface>) -> Status {
    let mut ota = axloader::ota::OtaController::open();
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
        let identity = match BootIdentity::establish(nic.mac_address) {
            Ok(identity) => {
                identity_failures = 0;
                identity
            }
            Err(error) => {
                if identity_failures < MAX_BOOTSTRAP_DIAGNOSTICS {
                    logln!("loader_identity_unavailable: {error:?}");
                }
                if !arch::RETRY_IDENTITY_FAILURE {
                    return error;
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
        let mut beacon = serial::SerialBeacon::new(identity.serial_id);
        let mut server = boot_server::BootServer::new(
            identity.epoch,
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

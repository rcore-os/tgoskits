//! Native RISC-V EFI control loop over the firmware-owned SNP controller.

use core::time::Duration;

use axloader::ota::OtaController;
use uefi::{Status, boot};

use crate::loader::{
    boot_server::{BootExecution, BootServer},
    fdt::Fdt,
    http::Action,
    identity::BootIdentity,
    serial::SerialBeacon,
    smbios,
    snp::Network,
};

pub(in crate::loader) fn run() -> Status {
    let _fdt = match Fdt::prepare() {
        Ok(fdt) => fdt,
        Err(error) => {
            crate::logln!("loader_fdt_unavailable: {error:?}");
            return error.status();
        }
    };
    let mut network = match Network::open() {
        Ok(network) => network,
        Err(error) => {
            crate::logln!("loader_snp_unavailable: {error:?}");
            return error.status();
        }
    };
    let mut ota = OtaController::open();
    let identity = match BootIdentity::establish(network.mac_address) {
        Ok(identity) => identity,
        Err(status) => {
            crate::logln!("loader_identity_unavailable: {status:?}");
            return close_before_return(network, status);
        }
    };
    let mut serial = SerialBeacon::new(identity.serial_id);
    let mut server = BootServer::new(
        identity.epoch,
        network.mac_address,
        network.current_mac_address,
        smbios::hardware_info(),
        serial.state.clone(),
    );
    crate::logln!(
        "loader_device_listening: port=2999 epoch={}",
        server.epoch()
    );
    loop {
        let action = match network.poll(&mut ota, &mut server, &mut serial) {
            Ok(action) => action,
            Err(error) => {
                crate::logln!("loader_snp_failed: {error:?}");
                return close_before_return(network, error.status());
            }
        };
        match action {
            Action::None => boot::stall(Duration::from_millis(1)),
            Action::Reset => {
                crate::logln!("ota_staged_reboot");
                if let Err(error) = network.close() {
                    crate::logln!("loader_snp_shutdown_failed: {error:?}");
                    reset(error.status());
                }
                drop(serial);
                reset(Status::SUCCESS);
            }
            Action::Boot(execution) => {
                let BootExecution { elf, handoff } = *execution;
                crate::logln!(
                    "elf_loaded: load={:#x} end={:#x} entry={:#x}",
                    elf.load_addr,
                    elf.load_end,
                    elf.entry_point
                );
                if let Err(error) = network.close() {
                    crate::logln!("loader_snp_shutdown_failed: {error:?}");
                    reset(error.status());
                }
                drop(serial);
                // Uploaded buffers have no role in the receiving kernel and
                // must be released before EBS. OTA only retains value data.
                drop(server);
                crate::logln!("ready_to_handoff");
                // SAFETY: validated ELF entry and allocation remain live; TCP
                // acknowledged the response, and SNP/UART resources are closed.
                // The shared handoff owner terminates instead of unwinding if a
                // child returns after invalidating Boot Services.
                let result = unsafe { handoff.enter(elf.entry_point) };
                drop(handoff);
                drop(elf);
                crate::logln!("jump_error: {result:?}");
                return Status::LOAD_ERROR;
            }
        }
    }
}

fn close_before_return(network: Network, status: Status) -> Status {
    if let Err(error) = network.close() {
        crate::logln!("loader_snp_shutdown_failed: {error:?}");
        reset(error.status());
    }
    status
}

fn reset(status: Status) -> ! {
    uefi::runtime::reset(uefi::runtime::ResetType::COLD, status, None)
}

#![cfg_attr(target_os = "uefi", no_std)]
#![cfg_attr(target_os = "uefi", no_main)]

#[cfg(not(target_os = "uefi"))]
fn main() {}

#[cfg(target_os = "uefi")]
mod firmware {

    extern crate alloc;

    use axloader::ota::{OtaDisk, Outcome, load_slot};
    use uefi::{Status, boot, prelude::*};

    #[entry]
    fn efi_main() -> Status {
        if uefi::helpers::init().is_err() {
            return Status::LOAD_ERROR;
        }
        match launch() {
            Ok(()) => Status::SUCCESS,
            Err(status) => {
                uefi::println!("axloader launcher: {status:?}; use recovery media");
                status
            }
        }
    }

    fn launch() -> Result<(), Status> {
        let mut disk = OtaDisk::open().map_err(|error| error.status())?;
        let (mut state, mut index) = disk.load().map_err(|error| error.status())?;
        if state.attempted {
            state = state
                .rollback(Outcome::RolledBack)
                .map_err(|_| Status::COMPROMISED_DATA)?;
            index = disk.commit(index, &state).map_err(|error| error.status())?;
        }
        let trial = state.pending;
        let candidate = trial.unwrap_or(state.active);
        let candidate_valid = disk
            .hash_slot(candidate)
            .is_ok_and(|digest| digest == *state.digest(candidate));
        let chosen = if candidate_valid {
            candidate
        } else if trial.is_some() {
            state = state
                .rollback(Outcome::LoadFailed)
                .map_err(|_| Status::COMPROMISED_DATA)?;
            disk.commit(index, &state).map_err(|error| error.status())?;
            state.active
        } else {
            let fallback = state.active.other();
            if *state.digest(fallback) == [0; 32] {
                return Err(Status::LOAD_ERROR);
            }
            if disk.hash_slot(fallback).map_err(|error| error.status())? != *state.digest(fallback)
            {
                return Err(Status::CRC_ERROR);
            }
            let mut recovered = state.clone();
            recovered.active = fallback;
            recovered.pending = None;
            recovered.outcome = Outcome::RolledBack;
            recovered.generation = recovered
                .generation
                .checked_add(1)
                .ok_or(Status::COMPROMISED_DATA)?;
            disk.commit(index, &recovered)
                .map_err(|error| error.status())?;
            state = recovered;
            fallback
        };
        if disk.hash_slot(chosen).map_err(|error| error.status())? != *state.digest(chosen) {
            return Err(Status::CRC_ERROR);
        }
        drop(disk);
        let image = match load_slot(chosen) {
            Ok(image) => image,
            Err(error) if chosen != state.active => {
                uefi::println!("axloader trial cannot load: {error:?}");
                return fallback_after_failure();
            }
            Err(error) => return Err(error.status()),
        };
        if chosen != state.active {
            let mut disk = OtaDisk::open().map_err(|error| error.status())?;
            let (current, index) = disk.load().map_err(|error| error.status())?;
            let attempted = current
                .mark_attempt()
                .map_err(|_| Status::COMPROMISED_DATA)?;
            disk.commit(index, &attempted)
                .map_err(|error| error.status())?;
        }
        let start = boot::start_image(image);
        let _ = boot::unload_image(image);
        // A trial image that returned to the launcher has failed; keep the stable
        // image intact and try it during this boot.
        if chosen != state.active {
            return fallback_after_failure();
        }
        Err(start
            .err()
            .map_or(Status::LOAD_ERROR, |error| error.status()))
    }

    fn fallback_after_failure() -> Result<(), Status> {
        let mut disk = OtaDisk::open().map_err(|error| error.status())?;
        let (state, index) = disk.load().map_err(|error| error.status())?;
        let previous = state.active;
        let failed = state
            .rollback(Outcome::LoadFailed)
            .map_err(|_| Status::COMPROMISED_DATA)?;
        disk.commit(index, &failed)
            .map_err(|error| error.status())?;
        if disk.hash_slot(previous).map_err(|error| error.status())? != *failed.digest(previous) {
            return Err(Status::CRC_ERROR);
        }
        drop(disk);
        let stable = load_slot(previous).map_err(|error| error.status())?;
        boot::start_image(stable).map_err(|error| error.status())
    }

    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
        loop {
            core::hint::spin_loop();
        }
    }
}

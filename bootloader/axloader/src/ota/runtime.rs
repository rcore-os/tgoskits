extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
};

use httpboot_protocol::{LoaderOtaState, OtaOutcome, OtaSource};
use sha2::{Digest, Sha256};
use uefi::{Status, boot, proto::loaded_image::LoadedImage};

use super::{InactiveWriter, MAX_IMAGE_BYTES, OtaDisk, Outcome, Slot, State, load_slot};

pub struct OtaController {
    state: State,
    slot: Slot,
    index: usize,
    last_failure: Option<&'static str>,
}

impl OtaController {
    pub fn open() -> Option<Self> {
        let image = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()).ok()?;
        let file_path = image
            .file_path()?
            .to_string16(
                uefi::proto::device_path::text::DisplayOnly(true),
                uefi::proto::device_path::text::AllowShortcuts(false),
            )
            .ok()?
            .to_string()
            .to_ascii_uppercase();
        let slot = if file_path.ends_with("A.EFI") {
            Slot::A
        } else if file_path.ends_with("B.EFI") {
            Slot::B
        } else {
            return None;
        };
        drop(image);
        let mut disk = OtaDisk::open().ok()?;
        let (state, index) = disk.load().ok()?;
        if slot != state.active && state.pending != Some(slot) {
            return None;
        }
        if disk.hash_slot(slot).ok()? != *state.digest(slot) {
            return None;
        }
        Some(Self {
            state,
            slot,
            index,
            last_failure: None,
        })
    }

    pub fn trial(&self) -> bool {
        self.state.pending == Some(self.slot) && self.state.attempted
    }

    pub fn record_failure(&mut self, reason: &'static str) {
        self.last_failure = Some(reason);
    }

    pub fn status_json(&self) -> serde_json::Value {
        serde_json::json!({
            "running_slot": match self.slot { Slot::A => "a", Slot::B => "b" },
            "running_sha256": hex(self.state.digest(self.slot)),
            "active_slot": match self.state.active { Slot::A => "a", Slot::B => "b" },
            "active_sha256": hex(self.state.digest(self.state.active)),
            "pending_update_id": self.pending_id(),
            "phase": if self.trial() { "awaiting_confirmation" } else if self.state.pending.is_some() { "staged" } else { "stable" },
            "version": self.state.version(),
            "trial": self.trial(),
            "source": self.state.source,
            "last_update_id": self.last_id(),
            "last_outcome": match self.state.outcome {
                Outcome::None => None,
                Outcome::RolledBack => Some("rolled_back"),
                Outcome::LoadFailed => Some("failed"),
                Outcome::Confirmed => Some("confirmed"),
            },
            "last_failure_reason": self.last_failure.or(match self.state.outcome {
                Outcome::RolledBack => Some("trial_not_confirmed_before_reset"),
                Outcome::LoadFailed => Some("trial_image_load_or_digest_failed"),
                _ => None,
            }),
        })
    }

    pub fn protocol_state(&self) -> LoaderOtaState {
        LoaderOtaState {
            active_sha256: hex(self.state.digest(self.state.active)),
            running_sha256: hex(self.state.digest(self.slot)),
            pending_update_id: self.pending_id(),
            trial: self.trial(),
            source: self.trial().then_some(self.state.source),
            last_update_id: self.last_id(),
            last_outcome: match self.state.outcome {
                Outcome::None => None,
                Outcome::RolledBack => Some(OtaOutcome::RolledBack),
                Outcome::LoadFailed => Some(OtaOutcome::Failed),
                Outcome::Confirmed => Some(OtaOutcome::Confirmed),
            },
        }
    }

    fn pending_id(&self) -> Option<String> {
        self.state.pending.map(|_| id_text(&self.state.update_id))
    }

    fn last_id(&self) -> Option<String> {
        (self.state.update_id != [0; 36]).then(|| id_text(&self.state.update_id))
    }

    pub fn next_direct_id(&self, digest: &[u8; 32]) -> [u8; 36] {
        let mut hash = Sha256::new();
        hash.update(self.state.generation.to_le_bytes());
        hash.update(self.state.digest(self.state.active));
        hash.update(digest);
        let bytes = hash.finalize();
        let value = hex(&bytes[..16]);
        let formatted = format!(
            "{}-{}-{}-{}-{}",
            &value[..8],
            &value[8..12],
            &value[12..16],
            &value[16..20],
            &value[20..32]
        );
        formatted
            .as_bytes()
            .try_into()
            .expect("UUID text always has 36 bytes")
    }

    pub fn start_update(&self, size: usize) -> Result<(OtaDisk, InactiveWriter), Status> {
        if self.trial() || self.state.pending.is_some() || size == 0 || size > MAX_IMAGE_BYTES {
            return Err(Status::ACCESS_DENIED);
        }
        let mut disk = OtaDisk::open().map_err(|error| error.status())?;
        let (current, _) = disk.load().map_err(|error| error.status())?;
        if current != self.state {
            return Err(Status::ACCESS_DENIED);
        }
        let writer = disk
            .start_inactive(&self.state, size)
            .map_err(|error| error.status())?;
        Ok((disk, writer))
    }

    pub fn finish_update(
        &mut self,
        mut disk: OtaDisk,
        writer: InactiveWriter,
        expected: [u8; 32],
        id: [u8; 36],
        source: OtaSource,
        version: Option<&str>,
    ) -> Result<(), Status> {
        if expected == *self.state.digest(self.state.active) {
            return Err(Status::INVALID_PARAMETER);
        }
        let slot = writer
            .finish(&mut disk, &expected)
            .map_err(|error| error.status())?;
        drop(disk);
        let image = load_slot(slot).map_err(|error| error.status())?;
        boot::unload_image(image).map_err(|error| error.status())?;
        let mut disk = OtaDisk::open().map_err(|error| error.status())?;
        let (state, index) = disk.load().map_err(|error| error.status())?;
        if state != self.state || index != self.index {
            return Err(Status::ACCESS_DENIED);
        }
        let next = state
            .stage_named(expected, id, source, version)
            .map_err(|_| Status::INVALID_PARAMETER)?;
        self.index = disk.commit(index, &next).map_err(|error| error.status())?;
        self.state = next;
        self.last_failure = None;
        Ok(())
    }

    pub fn confirm(&mut self, id: &str, source: OtaSource) -> Result<(), Status> {
        let id: [u8; 36] = id
            .as_bytes()
            .try_into()
            .map_err(|_| Status::INVALID_PARAMETER)?;
        let mut disk = OtaDisk::open().map_err(|error| error.status())?;
        let (state, index) = disk.load().map_err(|error| error.status())?;
        if state != self.state || index != self.index {
            return Err(Status::ACCESS_DENIED);
        }
        if disk.hash_slot(self.slot).map_err(|error| error.status())?
            != *self.state.digest(self.slot)
        {
            return Err(Status::CRC_ERROR);
        }
        let next = state
            .confirm(self.slot, &id, source)
            .map_err(|_| Status::ACCESS_DENIED)?;
        self.index = disk.commit(index, &next).map_err(|error| error.status())?;
        self.state = next;
        self.last_failure = None;
        Ok(())
    }
}

fn id_text(id: &[u8; 36]) -> String {
    core::str::from_utf8(id)
        .expect("validated OTA UUID")
        .to_string()
}

fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    use core::fmt::Write;
    for byte in bytes {
        write!(value, "{byte:02x}").expect("write to String");
    }
    value
}

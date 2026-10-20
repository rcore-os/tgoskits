//! Persistent A/B slots, state commits and inherited boot identity.
use std::{
    format,
    fs::{self, File, OpenOptions},
    io::Write,
    string::String,
    vec::Vec,
};

use axloader::{
    http_control::parse_update_id,
    ota::{self, Outcome, Slot, State},
};
use httpboot_protocol::{LoaderOtaState, OtaOutcome, OtaSource};
use sha2::{Digest, Sha256};
use someboot::handoff::ApplicationMetadata;

use super::{
    FIRMWARE_RESOURCE, hex,
    image::{LoadedPe, PublishedPayload, enter_prepared},
};

const OTA_METADATA_ID: [u8; 16] = *b"AXLOADER-OTA-V1!";
const OTA_ROOT: &str = "axloader";

pub(super) fn inherited_ota_slot(state: &State) -> Result<Option<u8>, &'static str> {
    let Some(metadata) = someboot::handoff::inherited_metadata() else {
        return Ok(None);
    };
    if metadata.id != OTA_METADATA_ID {
        return Ok(None);
    }
    if metadata.version != 1 {
        return Err("unsupported OTA metadata version");
    }
    let metadata = ota::handoff::OtaHandoff::decode(metadata.bytes)?;
    if metadata.generation != state.generation {
        return Err("inherited OTA generation mismatch");
    }
    Ok(Some(metadata.slot as u8))
}

#[derive(Debug)]
pub(super) enum OtaStageError {
    InvalidImage,
    Conflict,
    Storage(&'static str),
}

pub(super) struct LaOta {
    pub(super) state: State,
    index: usize,
    pub(super) slot: Slot,
    last_failure: Option<&'static str>,
}

impl LaOta {
    pub(super) fn open(current_image: &[u8]) -> Self {
        fs::create_dir_all(OTA_ROOT)
            .unwrap_or_else(|_| panic!("ota_bootstrap: failed to access OTA directory"));
        Self::cleanup_temporary_slots();
        let inherited = someboot::handoff::inherited_metadata().is_some();
        let (state, index) =
            match Self::load_state().unwrap_or_else(|error| panic!("ota_bootstrap: {error}")) {
                Some(state) => state,
                None => {
                    assert!(!inherited, "inherited OTA state is missing");
                    let path = Self::slot_path(Slot::A);
                    let digest = match Self::read_file_if_present(&path)
                        .unwrap_or_else(|error| panic!("ota_bootstrap: {error}"))
                    {
                        Some(bytes) => {
                            LoadedPe::validate(&bytes).unwrap_or_else(|error| {
                                panic!("ota_bootstrap: invalid stable A.EFI: {error}")
                            });
                            Sha256::digest(&bytes).into()
                        }
                        None => {
                            Self::persist_slot_image(Slot::A, current_image).unwrap_or_else(
                                |error| panic!("ota_bootstrap: failed to persist A.EFI: {error}"),
                            );
                            Sha256::digest(current_image).into()
                        }
                    };
                    let state = State::initial(digest).expect("nonzero initial OTA digest");
                    let index = Self::write_state(0, &state).unwrap_or_else(|error| {
                        panic!("ota_bootstrap: failed to persist state: {error}")
                    });
                    (state, index)
                }
            };
        Self {
            slot: state.active,
            state,
            index,
            last_failure: None,
        }
    }

    pub(super) fn set_running_slot(&mut self, slot: u8) -> Result<(), &'static str> {
        self.slot = match slot {
            0 => Slot::A,
            1 => Slot::B,
            _ => return Err("invalid inherited OTA slot"),
        };
        if self.state.pending == Some(self.slot) && !self.state.attempted {
            return Err("inherited OTA slot was not marked attempted");
        }
        Ok(())
    }

    pub(super) fn advance_boot(&mut self) -> Result<(), &'static str> {
        let next = self
            .state
            .advance_boot()
            .map_err(|_| "OTA boot generation overflow")?;
        self.commit(&next)
    }

    pub(super) fn boot_identity(&self, current_digest: [u8; 32]) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"axloader-loongarch-boot-v1");
        hash.update(self.state.generation.to_le_bytes());
        hash.update(current_digest);
        hash.finalize().into()
    }

    pub(super) fn prepare_bootstrap(
        &mut self,
        current_digest: [u8; 32],
    ) -> Result<Option<Slot>, &'static str> {
        if self.state.attempted {
            let next = self
                .state
                .rollback(Outcome::RolledBack)
                .map_err(|_| "invalid OTA rollback state")?;
            self.commit(&next)?;
        }
        let mut candidate = self.state.pending.unwrap_or(self.state.active);
        if !self.slot_matches_state(candidate)? {
            if self.state.pending == Some(candidate) {
                let next = self
                    .state
                    .rollback(Outcome::LoadFailed)
                    .map_err(|_| "invalid OTA candidate state")?;
                self.commit(&next)?;
                candidate = self.state.active;
            } else {
                return Err("active OTA slot is unavailable");
            }
        }
        if self.state.pending == Some(candidate) {
            let next = self
                .state
                .mark_attempt()
                .map_err(|_| "invalid OTA trial state")?;
            self.commit(&next)?;
        }
        self.slot = candidate;
        // The LoongArch EFI entry is the loader itself, rather than a separate
        // launcher. Continue in place when firmware already loaded the selected
        // slot; only invoke the slot image when the firmware image is different.
        if current_digest.as_slice() == self.state.digest(candidate) {
            Ok(None)
        } else {
            Ok(Some(candidate))
        }
    }

    fn slot_matches_state(&self, slot: Slot) -> Result<bool, &'static str> {
        let path = Self::slot_path(slot);
        let Some(bytes) = Self::read_file_if_present(&path)? else {
            return Ok(false);
        };
        Ok(Sha256::digest(&bytes).as_slice() == self.state.digest(slot))
    }

    fn persist_slot_image(slot: Slot, image: &[u8]) -> Result<(), &'static str> {
        // Preserve the parser's concrete reason so a bad firmware image or an
        // incorrect physical alias is diagnosable before it is persisted.
        LoadedPe::validate(image)?;
        let path = Self::slot_path(slot);
        let temporary = Self::slot_temporary_path(slot);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .map_err(|_| "initial slot create failed")?;
        file.write_all(image)
            .map_err(|_| "initial slot write failed")?;
        file.flush().map_err(|_| "initial slot flush failed")?;
        drop(file);
        // axstd's FAT rename does not replace an existing directory entry. Remove the
        // inactive slot only after the complete temporary image has been flushed; the
        // state record still points at the previous stable slot if the rename fails.
        let _ = fs::remove_file(&path);
        fs::rename(&temporary, &path).map_err(|_| "initial slot commit failed")?;
        let readback = fs::read(&path).map_err(|_| "initial slot readback failed")?;
        if readback != image || LoadedPe::validate(&readback).is_err() {
            return Err("initial slot readback mismatch");
        }
        Ok(())
    }

    pub(super) fn launch_slot(&mut self, slot: Slot) -> Result<(), &'static str> {
        let path = Self::slot_path(slot);
        let image = fs::read(&path).map_err(|_| "OTA slot read failed")?;
        if Sha256::digest(&image).as_slice() != self.state.digest(slot) {
            return Err("OTA slot digest mismatch");
        }
        let loaded = LoadedPe::load(&image)?;
        let bytes = ota::handoff::OtaHandoff {
            slot,
            generation: self.state.generation,
        }
        .encode();
        let payload = PublishedPayload::with_metadata(
            None,
            someboot::cmdline(),
            Some((loaded.load_addr, loaded.load_end)),
            Some(ApplicationMetadata {
                id: OTA_METADATA_ID,
                version: 1,
                bytes: &bytes,
            }),
        )?;
        enter_prepared(payload, loaded.entry_point, loaded)
    }

    fn load_state() -> Result<Option<(State, usize)>, &'static str> {
        let first_path = format!("{OTA_ROOT}/STATE0.BIN");
        let second_path = format!("{OTA_ROOT}/STATE1.BIN");
        let first = Self::read_file_if_present(&first_path)?;
        let second = Self::read_file_if_present(&second_path)?;
        if first.is_none() && second.is_none() {
            return Ok(None);
        }
        let first = first.unwrap_or_default();
        let second = second.unwrap_or_default();
        ota::newest(&first, &second)
            .map(Some)
            .map_err(|_| "both OTA state records are invalid")
    }

    fn read_file_if_present(path: &str) -> Result<Option<Vec<u8>>, &'static str> {
        let (directory, name) = path.rsplit_once('/').ok_or("OTA path has no directory")?;
        let entries = fs::read_dir(directory).map_err(|_| "OTA directory read failed")?;
        let mut present = false;
        for entry in entries {
            let entry = entry.map_err(|_| "OTA directory read failed")?;
            if entry.file_name().as_str().eq_ignore_ascii_case(name) {
                present = true;
                break;
            }
        }
        if !present {
            return Ok(None);
        }
        fs::read(path).map(Some).map_err(|_| "OTA file read failed")
    }

    fn write_state(index: usize, state: &State) -> Result<usize, &'static str> {
        let target = 1 - index;
        let path = format!("{OTA_ROOT}/STATE{target}.BIN");
        let encoded = state.encode();
        let mut file = File::create(&path).map_err(|_| "state create failed")?;
        file.write_all(&encoded).map_err(|_| "state write failed")?;
        file.flush().map_err(|_| "state flush failed")?;
        drop(file);
        let readback = fs::read(&path).map_err(|_| "state readback failed")?;
        let decoded = State::decode(&readback).map_err(|_| "state checksum failed")?;
        if decoded != *state || readback.as_slice() != encoded.as_slice() {
            return Err("state readback mismatch");
        }
        Ok(target)
    }

    fn commit(&mut self, state: &State) -> Result<(), &'static str> {
        self.index = Self::write_state(self.index, state)?;
        self.state = state.clone();
        Ok(())
    }

    pub(super) fn trial(&self) -> bool {
        self.state.pending == Some(self.slot) && self.state.attempted
    }

    pub(super) fn rollback(&mut self, outcome: Outcome) -> Result<(), &'static str> {
        let next = self
            .state
            .rollback(outcome)
            .map_err(|_| "invalid OTA rollback")?;
        self.commit(&next)?;
        self.slot = self.state.active;
        Ok(())
    }

    pub(super) fn next_direct_id(&self, digest: &[u8; 32]) -> [u8; 36] {
        let mut hash = Sha256::new();
        hash.update(self.state.generation.to_le_bytes());
        hash.update(self.state.digest(self.state.active));
        hash.update(digest);
        let value = hex(&hash.finalize()[..16]);
        format!(
            "{}-{}-{}-{}-{}",
            &value[..8],
            &value[8..12],
            &value[12..16],
            &value[16..20],
            &value[20..32]
        )
        .as_bytes()
        .try_into()
        .expect("UUID text has 36 bytes")
    }

    pub(super) fn stage_image(
        &mut self,
        image: &[u8],
        digest: [u8; 32],
        update_id: [u8; 36],
        version: Option<&str>,
        source: OtaSource,
    ) -> Result<(), OtaStageError> {
        if self.state.pending.is_some() {
            return Err(OtaStageError::Conflict);
        }
        if Sha256::digest(image).as_slice() != digest {
            return Err(OtaStageError::InvalidImage);
        }
        // Validate the image before replacing the inactive slot. A failed
        // upload must not leave an unbootable candidate behind.
        LoadedPe::validate(image).map_err(|_| OtaStageError::InvalidImage)?;
        let target = self.state.active.other();
        let path = Self::slot_path(target);
        let temporary = Self::slot_temporary_path(target);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .map_err(|_| OtaStageError::Storage("OTA image create failed"))?;
        file.write_all(image)
            .map_err(|_| OtaStageError::Storage("OTA image write failed"))?;
        file.flush()
            .map_err(|_| OtaStageError::Storage("OTA image flush failed"))?;
        drop(file);
        // Keep the replacement name within FAT's short-name format. The state is
        // committed only after readback validation, so an interrupted replacement
        // remains a rejected inactive candidate on the next boot.
        let _ = fs::remove_file(&path);
        fs::rename(&temporary, &path)
            .map_err(|_| OtaStageError::Storage("OTA image commit failed"))?;
        let readback =
            fs::read(&path).map_err(|_| OtaStageError::Storage("OTA image readback failed"))?;
        if Sha256::digest(&readback).as_slice() != digest || LoadedPe::validate(&readback).is_err()
        {
            return Err(OtaStageError::Storage("OTA image readback mismatch"));
        }
        let next = self
            .state
            .stage_named(digest, update_id, source, version)
            .map_err(|_| OtaStageError::Conflict)?;
        self.commit(&next).map_err(OtaStageError::Storage)
    }

    pub(super) fn confirm(
        &mut self,
        update_id: &str,
        source: OtaSource,
    ) -> Result<(), &'static str> {
        let id = parse_update_id(update_id).ok_or("invalid update id")?;
        let next = self
            .state
            .confirm(self.slot, &id, source)
            .map_err(|_| "stale OTA confirmation")?;
        self.commit(&next)
    }

    pub(super) fn status_json(&self) -> serde_json::Value {
        serde_json::json!({
            "running_slot": slot_text(self.slot),
            "active_slot": slot_text(self.state.active),
            "pending_update_id": self.state.pending.map(|_| core::str::from_utf8(&self.state.update_id).unwrap_or("")),
            "phase": if self.trial() { "awaiting_confirmation" } else if self.state.pending.is_some() { "staged" } else { "stable" },
            "trial": self.trial(),
            "source": self.state.source,
            "last_outcome": outcome_text(self.state.outcome),
            "last_failure_reason": self.last_failure,
        })
    }

    pub(super) fn protocol_state(&self) -> LoaderOtaState {
        LoaderOtaState {
            active_sha256: hex(self.state.digest(self.state.active)),
            running_sha256: hex(self.state.digest(self.slot)),
            pending_update_id: self.state.pending.map(|_| {
                core::str::from_utf8(&self.state.update_id)
                    .unwrap_or("")
                    .into()
            }),
            trial: self.trial(),
            source: self.trial().then_some(self.state.source),
            last_update_id: (self.state.update_id != [0; 36]).then(|| {
                core::str::from_utf8(&self.state.update_id)
                    .unwrap_or("")
                    .into()
            }),
            last_outcome: match self.state.outcome {
                Outcome::None => None,
                Outcome::RolledBack => Some(OtaOutcome::RolledBack),
                Outcome::LoadFailed => Some(OtaOutcome::Failed),
                Outcome::Confirmed => Some(OtaOutcome::Confirmed),
            },
        }
    }

    fn slot_path(slot: Slot) -> String {
        format!("{OTA_ROOT}/{}.EFI", slot_text(slot).to_ascii_uppercase())
    }

    fn slot_temporary_path(slot: Slot) -> String {
        format!("{OTA_ROOT}/{}.NEW", slot_text(slot).to_ascii_uppercase())
    }

    fn cleanup_temporary_slots() {
        for slot in [Slot::A, Slot::B] {
            let _ = fs::remove_file(&Self::slot_temporary_path(slot));
            // Remove the long-name temporary path used by the first implementation.
            let _ = fs::remove_file(&format!("{}.new", Self::slot_path(slot)));
        }
    }
}

pub(super) fn firmware_boot_image() -> Option<Vec<u8>> {
    // Preserve the original file before EBS. Runtime data and relocated pointers
    // cannot be used to reconstruct a restartable EFI application.
    let image = someboot::boot_resources::initial_contents(FIRMWARE_RESOURCE)?;
    LoadedPe::validate(image).ok()?;
    Some(image.to_vec())
}

fn slot_text(slot: Slot) -> &'static str {
    match slot {
        Slot::A => "a",
        Slot::B => "b",
    }
}

fn outcome_text(outcome: Outcome) -> Option<&'static str> {
    match outcome {
        Outcome::None => None,
        Outcome::RolledBack => Some("rolled_back"),
        Outcome::LoadFailed => Some("failed"),
        Outcome::Confirmed => Some("confirmed"),
    }
}

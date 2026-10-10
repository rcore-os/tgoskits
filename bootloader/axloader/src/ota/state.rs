//! On-disk OTA transaction records shared by the launcher and the loader.
//!
//! The records are written alternately. A record is never considered committed
//! until its complete checksum has been written and the file has been flushed.

use httpboot_protocol::OtaSource;
use sha2::{Digest, Sha256};

pub const RECORD_SIZE: usize = 256;
const CHECKSUM_START: usize = RECORD_SIZE - 32;
const MAGIC: &[u8; 8] = b"AXOTA001";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    const fn as_byte(self) -> u8 {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }

    fn from_byte(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::A),
            1 => Some(Self::B),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    None,
    RolledBack,
    LoadFailed,
    Confirmed,
}

impl Outcome {
    const fn as_byte(self) -> u8 {
        match self {
            Self::None => 0,
            Self::RolledBack => 1,
            Self::LoadFailed => 2,
            Self::Confirmed => 3,
        }
    }

    fn from_byte(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::RolledBack),
            2 => Some(Self::LoadFailed),
            3 => Some(Self::Confirmed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct State {
    pub generation: u64,
    pub active: Slot,
    pub pending: Option<Slot>,
    pub attempted: bool,
    pub source: OtaSource,
    pub digests: [[u8; 32]; 2],
    pub update_id: [u8; 36],
    pub outcome: Outcome,
    pub display_version: [u8; 96],
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StateError {
    Invalid,
    Busy,
    Stale,
    Overflow,
}

impl State {
    pub fn initial(active_digest: [u8; 32]) -> Result<Self, StateError> {
        if active_digest == [0; 32] {
            return Err(StateError::Invalid);
        }
        Ok(Self {
            generation: 1,
            active: Slot::A,
            pending: None,
            attempted: false,
            source: OtaSource::Direct,
            digests: [active_digest, [0; 32]],
            update_id: [0; 36],
            outcome: Outcome::None,
            display_version: [0; 96],
        })
    }

    pub fn digest(&self, slot: Slot) -> &[u8; 32] {
        &self.digests[slot.as_byte() as usize]
    }

    pub fn stage(
        &self,
        digest: [u8; 32],
        update_id: [u8; 36],
        source: OtaSource,
    ) -> Result<Self, StateError> {
        self.stage_named(digest, update_id, source, None)
    }

    pub fn stage_named(
        &self,
        digest: [u8; 32],
        update_id: [u8; 36],
        source: OtaSource,
        version: Option<&str>,
    ) -> Result<Self, StateError> {
        if self.pending.is_some() {
            return Err(StateError::Busy);
        }
        if version.is_some_and(|value| {
            value.len() > 96 || !value.bytes().all(|byte| byte.is_ascii_graphic())
        }) {
            return Err(StateError::Invalid);
        }
        if digest == [0; 32] || digest == *self.digest(self.active) || !valid_id(&update_id) {
            return Err(StateError::Invalid);
        }
        let mut next = self.next()?;
        let slot = self.active.other();
        next.digests[slot.as_byte() as usize] = digest;
        next.pending = Some(slot);
        next.attempted = false;
        next.source = source;
        next.update_id = update_id;
        next.outcome = Outcome::None;
        next.display_version = [0; 96];
        if let Some(version) = version {
            next.display_version[..version.len()].copy_from_slice(version.as_bytes());
        }
        Ok(next)
    }

    pub fn version(&self) -> Option<&str> {
        let len = self
            .display_version
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(96);
        (len > 0).then(|| {
            core::str::from_utf8(&self.display_version[..len]).expect("validated OTA version")
        })
    }

    pub fn mark_attempt(&self) -> Result<Self, StateError> {
        if self.pending.is_none() || self.attempted {
            return Err(StateError::Invalid);
        }
        let mut next = self.next()?;
        next.attempted = true;
        Ok(next)
    }

    pub fn confirm(
        &self,
        running: Slot,
        update_id: &[u8; 36],
        source: OtaSource,
    ) -> Result<Self, StateError> {
        if self.pending != Some(running)
            || !self.attempted
            || self.update_id != *update_id
            || self.source != source
        {
            return Err(StateError::Stale);
        }
        let mut next = self.next()?;
        next.active = running;
        next.pending = None;
        next.attempted = false;
        next.outcome = Outcome::Confirmed;
        Ok(next)
    }

    pub fn rollback(&self, outcome: Outcome) -> Result<Self, StateError> {
        let Some(pending) = self.pending else {
            return Err(StateError::Invalid);
        };
        if !matches!(outcome, Outcome::RolledBack | Outcome::LoadFailed) {
            return Err(StateError::Invalid);
        }
        let mut next = self.next()?;
        // The abandoned image was never confirmed. Its digest must not make
        // it eligible as a fallback if the stable slot fails later.
        next.digests[pending.as_byte() as usize] = [0; 32];
        next.pending = None;
        next.attempted = false;
        next.outcome = outcome;
        Ok(next)
    }

    fn next(&self) -> Result<Self, StateError> {
        let mut next = self.clone();
        next.generation = next.generation.checked_add(1).ok_or(StateError::Overflow)?;
        Ok(next)
    }

    pub fn encode(&self) -> [u8; RECORD_SIZE] {
        let mut bytes = [0; RECORD_SIZE];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..16].copy_from_slice(&self.generation.to_le_bytes());
        bytes[16] = self.active.as_byte();
        bytes[17] = self.pending.map_or(0xff, Slot::as_byte);
        bytes[18] = u8::from(self.attempted);
        bytes[19] = match self.source {
            OtaSource::Direct => 0,
            OtaSource::Server => 1,
        };
        bytes[20..52].copy_from_slice(&self.digests[0]);
        bytes[52..84].copy_from_slice(&self.digests[1]);
        bytes[84..120].copy_from_slice(&self.update_id);
        bytes[120] = self.outcome.as_byte();
        bytes[121..217].copy_from_slice(&self.display_version);
        let checksum = Sha256::digest(&bytes[..CHECKSUM_START]);
        bytes[CHECKSUM_START..].copy_from_slice(&checksum);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, StateError> {
        if bytes.len() != RECORD_SIZE
            || &bytes[..8] != MAGIC
            || Sha256::digest(&bytes[..CHECKSUM_START]).as_slice() != &bytes[CHECKSUM_START..]
            || bytes[217..CHECKSUM_START].iter().any(|&byte| byte != 0)
        {
            return Err(StateError::Invalid);
        }
        let state = Self {
            generation: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            active: Slot::from_byte(bytes[16]).ok_or(StateError::Invalid)?,
            pending: match bytes[17] {
                0xff => None,
                value => Some(Slot::from_byte(value).ok_or(StateError::Invalid)?),
            },
            attempted: match bytes[18] {
                0 => false,
                1 => true,
                _ => return Err(StateError::Invalid),
            },
            source: match bytes[19] {
                0 => OtaSource::Direct,
                1 => OtaSource::Server,
                _ => return Err(StateError::Invalid),
            },
            digests: [
                bytes[20..52].try_into().unwrap(),
                bytes[52..84].try_into().unwrap(),
            ],
            update_id: bytes[84..120].try_into().unwrap(),
            outcome: Outcome::from_byte(bytes[120]).ok_or(StateError::Invalid)?,
            display_version: bytes[121..217].try_into().unwrap(),
        };
        if state.generation == 0
            || state.digest(state.active) == &[0; 32]
            || state
                .pending
                .is_some_and(|pending| pending == state.active || state.digest(pending) == &[0; 32])
            || (state.pending.is_some() && !valid_id(&state.update_id))
            || (state.pending.is_none() && state.attempted)
            || state
                .display_version
                .iter()
                .any(|byte| *byte != 0 && !byte.is_ascii_graphic())
            || state
                .display_version
                .iter()
                .skip_while(|byte| **byte != 0)
                .any(|byte| *byte != 0)
        {
            return Err(StateError::Invalid);
        }
        Ok(state)
    }
}

#[cfg(any(target_os = "uefi", test))]
pub(super) fn newest(first: &[u8], second: &[u8]) -> Result<(State, usize), StateError> {
    match (State::decode(first), State::decode(second)) {
        (Ok(a), Ok(b)) if b.generation > a.generation => Ok((b, 1)),
        (Ok(a), _) => Ok((a, 0)),
        (_, Ok(b)) => Ok((b, 1)),
        _ => Err(StateError::Invalid),
    }
}

fn valid_id(value: &[u8; 36]) -> bool {
    value.iter().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            *byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_trial_restores_the_stable_image() {
        let old = State::initial([1; 32]).unwrap();
        let id = *b"01234567-89ab-cdef-0123-456789abcdef";
        let staged = old.stage([2; 32], id, OtaSource::Direct).unwrap();
        let attempting = staged.mark_attempt().unwrap();
        assert_eq!(
            attempting.confirm(Slot::A, &id, OtaSource::Direct),
            Err(StateError::Stale)
        );
        assert_eq!(
            attempting.confirm(Slot::B, &id, OtaSource::Server),
            Err(StateError::Stale)
        );
        let old_record = staged.encode();
        let attempted_record = attempting.encode();
        assert_eq!(
            newest(&old_record, &attempted_record).unwrap().0,
            attempting
        );
        let recovered = attempting.rollback(Outcome::RolledBack).unwrap();
        assert_eq!(recovered.active, Slot::A);
        assert!(recovered.pending.is_none());
        assert_eq!(recovered.digest(Slot::A), &[1; 32]);
        assert_eq!(recovered.digest(Slot::B), &[0; 32]);
        assert_eq!(
            attempting
                .rollback(Outcome::LoadFailed)
                .unwrap()
                .digest(Slot::B),
            &[0; 32]
        );
        let confirmed = attempting.confirm(Slot::B, &id, OtaSource::Direct).unwrap();
        assert_eq!(confirmed.active, Slot::B);
        assert_eq!(confirmed.digest(Slot::A), &[1; 32]);
        assert_eq!(
            confirmed
                .stage([3; 32], id, OtaSource::Server)
                .unwrap()
                .pending,
            Some(Slot::A)
        );
    }

    #[test]
    fn damaged_record_cannot_override_a_valid_generation() {
        let old = State::initial([1; 32]).unwrap().encode();
        let mut torn = State::initial([2; 32]).unwrap().encode();
        torn[20] ^= 0x80;
        assert_eq!(newest(&old, &torn).unwrap().0.active, Slot::A);
        assert!(newest(&torn, &torn).is_err());
    }
}

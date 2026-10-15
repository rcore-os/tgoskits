//! axloader-owned OTA metadata. someboot transports these opaque bytes.
use super::Slot;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OtaHandoff {
    pub slot: Slot,
    pub generation: u64,
}
impl OtaHandoff {
    pub fn encode(self) -> [u8; 9] {
        let mut bytes = [0; 9];
        bytes[0] = self.slot as u8;
        bytes[1..].copy_from_slice(&self.generation.to_le_bytes());
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() != 9 {
            return Err("invalid OTA metadata length");
        }
        let slot = match bytes[0] {
            0 => Slot::A,
            1 => Slot::B,
            _ => return Err("invalid OTA metadata slot"),
        };
        Ok(Self {
            slot,
            generation: u64::from_le_bytes(bytes[1..].try_into().unwrap()),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ota_metadata_is_exact_and_application_owned() {
        let value = OtaHandoff {
            slot: Slot::B,
            generation: 17,
        };
        assert_eq!(OtaHandoff::decode(&value.encode()), Ok(value));
        assert!(OtaHandoff::decode(&[2; 9]).is_err());
        assert!(OtaHandoff::decode(&[0; 10]).is_err());
    }
}

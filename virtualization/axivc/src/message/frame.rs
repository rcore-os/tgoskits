use super::{IvcMessageError, IvcMessageId};
use crate::{IVC_MESSAGE_HEADER_SIZE, IVC_SLOT_FRAGMENT_CAPACITY, IVC_SLOT_SIZE};

const MESSAGE_VERSION_V1: u8 = 1;
const VERSION_OFFSET: usize = 0;
const FLAGS_OFFSET: usize = 1;
const HEADER_LEN_OFFSET: usize = 2;
const FRAGMENT_LEN_OFFSET: usize = 4;
const MESSAGE_ID_OFFSET: usize = 8;
const MESSAGE_LEN_OFFSET: usize = 16;

const FLAG_FIRST: u8 = 1 << 0;
const FLAG_LAST: u8 = 1 << 1;
const FLAG_ABORT: u8 = 1 << 2;
const KNOWN_FLAGS: u8 = FLAG_FIRST | FLAG_LAST | FLAG_ABORT;

pub(super) struct FrameSpec {
    pub(super) message_id: IvcMessageId,
    pub(super) message_len: u64,
    pub(super) first: bool,
    pub(super) last: bool,
    pub(super) abort: bool,
}

#[derive(Debug)]
pub(super) struct DecodedFrame<'a> {
    pub(super) message_id: IvcMessageId,
    pub(super) message_len: u64,
    pub(super) fragment: &'a [u8],
    pub(super) first: bool,
    pub(super) last: bool,
    pub(super) abort: bool,
}

pub(super) fn encode_frame(
    slot: &mut [u8; IVC_SLOT_SIZE],
    spec: FrameSpec,
    fragment: &[u8],
) -> Result<(), IvcMessageError> {
    validate_outgoing_frame(&spec, fragment)?;

    slot.fill(0);
    slot[VERSION_OFFSET] = MESSAGE_VERSION_V1;
    slot[FLAGS_OFFSET] = encode_flags(&spec);
    slot[HEADER_LEN_OFFSET..FRAGMENT_LEN_OFFSET]
        .copy_from_slice(&(IVC_MESSAGE_HEADER_SIZE as u16).to_le_bytes());
    slot[FRAGMENT_LEN_OFFSET..MESSAGE_ID_OFFSET]
        .copy_from_slice(&(fragment.len() as u32).to_le_bytes());
    slot[MESSAGE_ID_OFFSET..MESSAGE_LEN_OFFSET]
        .copy_from_slice(&spec.message_id.get().to_le_bytes());
    slot[MESSAGE_LEN_OFFSET..IVC_MESSAGE_HEADER_SIZE]
        .copy_from_slice(&spec.message_len.to_le_bytes());
    slot[IVC_MESSAGE_HEADER_SIZE..IVC_MESSAGE_HEADER_SIZE + fragment.len()]
        .copy_from_slice(fragment);
    Ok(())
}

pub(super) fn decode_frame(
    slot: &[u8; IVC_SLOT_SIZE],
) -> Result<DecodedFrame<'_>, IvcMessageError> {
    let version = slot[VERSION_OFFSET];
    if version != MESSAGE_VERSION_V1 {
        return Err(IvcMessageError::UnsupportedVersion { version });
    }

    let flags = slot[FLAGS_OFFSET];
    if flags & !KNOWN_FLAGS != 0 {
        return Err(IvcMessageError::UnknownFlags { flags });
    }

    let header_len = read_u16(slot, HEADER_LEN_OFFSET) as usize;
    if header_len != IVC_MESSAGE_HEADER_SIZE {
        return Err(IvcMessageError::MalformedHeader);
    }

    let fragment_len = usize::try_from(read_u32(slot, FRAGMENT_LEN_OFFSET))
        .map_err(|_| IvcMessageError::MalformedHeader)?;
    let fragment_capacity = IVC_SLOT_SIZE - header_len;
    if fragment_len > fragment_capacity {
        return Err(IvcMessageError::FragmentTooLarge {
            length: fragment_len,
            capacity: fragment_capacity,
        });
    }

    let raw_message_id = read_u64(slot, MESSAGE_ID_OFFSET);
    let message_id = IvcMessageId::new(raw_message_id).ok_or(IvcMessageError::MalformedHeader)?;
    let message_len = read_u64(slot, MESSAGE_LEN_OFFSET);
    let first = flags & FLAG_FIRST != 0;
    let last = flags & FLAG_LAST != 0;
    let abort = flags & FLAG_ABORT != 0;
    validate_decoded_flags(message_len, fragment_len, first, last, abort)?;

    Ok(DecodedFrame {
        message_id,
        message_len,
        fragment: &slot[header_len..header_len + fragment_len],
        first,
        last,
        abort,
    })
}

fn validate_outgoing_frame(spec: &FrameSpec, fragment: &[u8]) -> Result<(), IvcMessageError> {
    if fragment.len() > IVC_SLOT_FRAGMENT_CAPACITY {
        return Err(IvcMessageError::FragmentTooLarge {
            length: fragment.len(),
            capacity: IVC_SLOT_FRAGMENT_CAPACITY,
        });
    }
    validate_decoded_flags(
        spec.message_len,
        fragment.len(),
        spec.first,
        spec.last,
        spec.abort,
    )
}

fn validate_decoded_flags(
    message_len: u64,
    fragment_len: usize,
    first: bool,
    last: bool,
    abort: bool,
) -> Result<(), IvcMessageError> {
    if abort {
        if first || last || fragment_len != 0 {
            return Err(IvcMessageError::MalformedHeader);
        }
        return Ok(());
    }

    if message_len == 0 {
        if !first || !last || fragment_len != 0 {
            return Err(IvcMessageError::MalformedHeader);
        }
    } else if fragment_len == 0 {
        return Err(IvcMessageError::MalformedHeader);
    }
    Ok(())
}

fn encode_flags(spec: &FrameSpec) -> u8 {
    let first = if spec.first { FLAG_FIRST } else { 0 };
    let last = if spec.last { FLAG_LAST } else { 0 };
    let abort = if spec.abort { FLAG_ABORT } else { 0 };
    first | last | abort
}

fn read_u16(slot: &[u8; IVC_SLOT_SIZE], offset: usize) -> u16 {
    u16::from_le_bytes([slot[offset], slot[offset + 1]])
}

fn read_u32(slot: &[u8; IVC_SLOT_SIZE], offset: usize) -> u32 {
    u32::from_le_bytes([
        slot[offset],
        slot[offset + 1],
        slot[offset + 2],
        slot[offset + 3],
    ])
}

fn read_u64(slot: &[u8; IVC_SLOT_SIZE], offset: usize) -> u64 {
    u64::from_le_bytes([
        slot[offset],
        slot[offset + 1],
        slot[offset + 2],
        slot[offset + 3],
        slot[offset + 4],
        slot[offset + 5],
        slot[offset + 6],
        slot[offset + 7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_uses_the_frozen_little_endian_v1_layout() {
        let mut slot = [0u8; IVC_SLOT_SIZE];
        let message_id = IvcMessageId::new(0x0807_0605_0403_0201).unwrap();
        encode_frame(
            &mut slot,
            FrameSpec {
                message_id,
                message_len: 0x1817_1615_1413_1211,
                first: true,
                last: true,
                abort: false,
            },
            b"abc",
        )
        .unwrap();

        assert_eq!(
            &slot[..27],
            &[
                1, 3, 24, 0, 3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 17, 18, 19, 20, 21, 22, 23, 24,
                b'a', b'b', b'c',
            ]
        );
        let frame = decode_frame(&slot).unwrap();
        assert_eq!(frame.message_id, message_id);
        assert_eq!(frame.message_len, 0x1817_1615_1413_1211);
        assert_eq!(frame.fragment, b"abc");
        assert!(frame.first);
        assert!(frame.last);
    }

    #[test]
    fn codec_rejects_unknown_version_flags_and_oversized_fragments() {
        let mut slot = valid_slot();
        slot[VERSION_OFFSET] = 2;
        assert_eq!(
            decode_frame(&slot).unwrap_err(),
            IvcMessageError::UnsupportedVersion { version: 2 }
        );

        let mut slot = valid_slot();
        slot[FLAGS_OFFSET] |= 0x80;
        assert_eq!(
            decode_frame(&slot).unwrap_err(),
            IvcMessageError::UnknownFlags { flags: 0x83 }
        );

        let mut slot = valid_slot();
        let oversized_length = IVC_SLOT_FRAGMENT_CAPACITY + 1;
        slot[FRAGMENT_LEN_OFFSET..MESSAGE_ID_OFFSET]
            .copy_from_slice(&(oversized_length as u32).to_le_bytes());
        assert_eq!(
            decode_frame(&slot).unwrap_err(),
            IvcMessageError::FragmentTooLarge {
                length: oversized_length,
                capacity: IVC_SLOT_FRAGMENT_CAPACITY,
            }
        );
    }

    #[test]
    fn codec_rejects_invalid_header_and_abort_shapes() {
        let mut slot = valid_slot();
        slot[HEADER_LEN_OFFSET..FRAGMENT_LEN_OFFSET].copy_from_slice(&23u16.to_le_bytes());
        assert_eq!(
            decode_frame(&slot).unwrap_err(),
            IvcMessageError::MalformedHeader
        );

        let mut slot = valid_slot();
        slot[HEADER_LEN_OFFSET..FRAGMENT_LEN_OFFSET]
            .copy_from_slice(&((IVC_SLOT_SIZE + 1) as u16).to_le_bytes());
        assert_eq!(
            decode_frame(&slot).unwrap_err(),
            IvcMessageError::MalformedHeader
        );

        let mut slot = valid_slot();
        slot[FLAGS_OFFSET] = FLAG_ABORT;
        assert_eq!(
            decode_frame(&slot).unwrap_err(),
            IvcMessageError::MalformedHeader
        );
    }

    #[test]
    fn arbitrary_slots_never_panic_during_decode() {
        for seed in 0u16..=u8::MAX as u16 {
            let mut slot = [0u8; IVC_SLOT_SIZE];
            for (index, byte) in slot.iter_mut().enumerate() {
                *byte = (seed as u8).wrapping_mul(31).wrapping_add(index as u8);
            }
            let _ = decode_frame(&slot);
        }
    }

    fn valid_slot() -> [u8; IVC_SLOT_SIZE] {
        let mut slot = [0u8; IVC_SLOT_SIZE];
        encode_frame(
            &mut slot,
            FrameSpec {
                message_id: IvcMessageId::new(1).unwrap(),
                message_len: 1,
                first: true,
                last: true,
                abort: false,
            },
            b"x",
        )
        .unwrap();
        slot
    }
}

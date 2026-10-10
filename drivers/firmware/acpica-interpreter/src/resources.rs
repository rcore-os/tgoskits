//! Checked AML resource-template decoding for interrupt links and EC ports.
use alloc::vec::Vec;

use crate::{BAD_PARAMETER, NO_MEMORY, Status};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Irq {
    pub numbers: Vec<u32>,
    pub level: bool,
    pub active_low: bool,
    pub shared: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resources {
    pub irqs: Vec<Irq>,
    pub io: Vec<(u16, u8)>,
}
fn push<T>(v: &mut Vec<T>, item: T) -> Result<(), Status> {
    v.try_reserve(1).map_err(|_| NO_MEMORY)?;
    v.push(item);
    Ok(())
}
pub fn parse(bytes: &[u8]) -> Result<Resources, Status> {
    let mut result = Resources::default();
    let mut at = 0;
    while at < bytes.len() {
        let tag = bytes[at];
        at += 1;
        let (kind, len) = if tag & 0x80 != 0 {
            let size = bytes.get(at..at + 2).ok_or(BAD_PARAMETER)?;
            at += 2;
            (
                tag,
                usize::from(u16::from_le_bytes(size.try_into().unwrap())),
            )
        } else {
            (tag >> 3, usize::from(tag & 7))
        };
        let b = bytes
            .get(at..at.checked_add(len).ok_or(BAD_PARAMETER)?)
            .ok_or(BAD_PARAMETER)?;
        at += len;
        match kind {
            0xf => {
                if len != 1 || at != bytes.len() {
                    return Err(BAD_PARAMETER);
                }
                if b[0] != 0 && bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)) != 0 {
                    return Err(BAD_PARAMETER);
                }
                return Ok(result);
            }
            4 => {
                if !matches!(len, 2 | 3) {
                    return Err(BAD_PARAMETER);
                }
                let mask = u16::from_le_bytes(b[..2].try_into().unwrap());
                let flags = b.get(2).copied().unwrap_or(1);
                let mut nums = Vec::new();
                for i in 0..16 {
                    if mask & (1 << i) != 0 {
                        push(&mut nums, i)?;
                    }
                }
                push(
                    &mut result.irqs,
                    Irq {
                        numbers: nums,
                        level: flags & 1 == 0,
                        active_low: flags & 8 != 0,
                        shared: flags & 16 != 0,
                    },
                )?;
            }
            8 => {
                if len != 7 {
                    return Err(BAD_PARAMETER);
                }
                let min = u16::from_le_bytes(b[1..3].try_into().unwrap());
                let max = u16::from_le_bytes(b[3..5].try_into().unwrap());
                if min != max || u32::from(min) + u32::from(b[6]) > 65536 {
                    return Err(BAD_PARAMETER);
                }
                push(&mut result.io, (min, b[6]))?;
            }
            9 => {
                if len != 3 {
                    return Err(BAD_PARAMETER);
                }
                let min = u16::from_le_bytes(b[..2].try_into().unwrap());
                if u32::from(min) + u32::from(b[2]) > 65536 {
                    return Err(BAD_PARAMETER);
                }
                push(&mut result.io, (min, b[2]))?;
            }
            0x89 => {
                if len < 2 {
                    return Err(BAD_PARAMETER);
                }
                let flags = b[0];
                let count = usize::from(b[1]);
                if count == 0 || len < 2 + count * 4 {
                    return Err(BAD_PARAMETER);
                }
                let mut nums = Vec::new();
                for value in b[2..2 + count * 4].as_chunks::<4>().0 {
                    push(&mut nums, u32::from_le_bytes(*value))?;
                }
                push(
                    &mut result.irqs,
                    Irq {
                        numbers: nums,
                        level: flags & 2 == 0,
                        active_low: flags & 4 != 0,
                        shared: flags & 8 != 0,
                    },
                )?;
            }
            // Other descriptors are length-checked and skipped. They cannot
            // fabricate an IRQ/EC port when an unsupported resource is present.
            _ => {}
        }
    }
    Err(BAD_PARAMETER)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ec_io_and_irq() {
        let b = [
            0x47, 1, 0x62, 0, 0x62, 0, 1, 1, 0x4b, 0x66, 0, 1, 0x23, 0, 2, 0x18, 0x79, 0,
        ];
        let r = parse(&b).unwrap();
        assert_eq!(r.io, [(0x62, 1), (0x66, 1)]);
        assert_eq!(r.irqs[0].numbers, [9]);
        assert!(r.irqs[0].level && r.irqs[0].active_low && r.irqs[0].shared);
        for end in 0..b.len() {
            assert!(parse(&b[..end]).is_err());
        }
    }
    #[test]
    fn rejects_overflow_and_variable_ports() {
        assert!(parse(&[0x4b, 0xff, 0xff, 2, 0x79, 0]).is_err());
        assert!(parse(&[0x47, 1, 0x62, 0, 0x63, 0, 1, 1, 0x79, 0]).is_err());
    }
    #[test]
    fn extended_irq() {
        let r = parse(&[0x89, 6, 0, 0x0c, 1, 0x15, 0, 0, 0, 0x79, 0]).unwrap();
        assert_eq!(r.irqs[0].numbers, [21]);
        assert!(r.irqs[0].level && r.irqs[0].active_low && r.irqs[0].shared);
    }
}

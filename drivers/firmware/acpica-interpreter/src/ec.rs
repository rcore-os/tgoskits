//! Original bounded, polling ACPI embedded-controller byte protocol.
use crate::{BAD_PARAMETER, Status, TIME};
pub trait Io {
    fn status(&mut self) -> Result<u8, Status>;
    fn read_data(&mut self) -> Result<u8, Status>;
    fn command(&mut self, value: u8) -> Result<(), Status>;
    fn write_data(&mut self, value: u8) -> Result<(), Status>;
    fn now_us(&self) -> u64;
    fn stall_us(&mut self, micros: u32);
}
fn wait(io: &mut impl Io, mask: u8, set: bool) -> Result<(), Status> {
    let start = io.now_us();
    loop {
        if (io.status()? & mask != 0) == set {
            return Ok(());
        }
        if io.now_us().wrapping_sub(start) >= 100_000 {
            return Err(TIME);
        }
        io.stall_us(10);
    }
}
pub fn read(io: &mut impl Io, address: u8) -> Result<u8, Status> {
    wait(io, 2, false)?;
    io.command(0x80)?;
    wait(io, 2, false)?;
    io.write_data(address)?;
    wait(io, 1, true)?;
    io.read_data()
}
pub fn write(io: &mut impl Io, address: u8, value: u8) -> Result<(), Status> {
    wait(io, 2, false)?;
    io.command(0x81)?;
    wait(io, 2, false)?;
    io.write_data(address)?;
    wait(io, 2, false)?;
    io.write_data(value)?;
    wait(io, 2, false)
}
pub fn query(io: &mut impl Io) -> Result<Option<u8>, Status> {
    if io.status()? & 0x20 == 0 {
        return Ok(None);
    }
    wait(io, 2, false)?;
    io.command(0x84)?;
    wait(io, 1, true)?;
    let query = io.read_data()?;
    Ok((query != 0).then_some(query))
}
pub fn transfer(
    io: &mut impl Io,
    write_op: bool,
    address: u64,
    width: u32,
    value: &mut u64,
) -> Result<(), Status> {
    if !matches!(width, 8 | 16 | 32 | 64)
        || address
            .checked_add(u64::from(width / 8))
            .is_none_or(|end| end > 256)
    {
        return Err(BAD_PARAMETER);
    }
    if !write_op {
        *value = 0;
    }
    for i in 0..width / 8 {
        let addr = (address + u64::from(i)) as u8;
        if write_op {
            write(io, addr, (*value >> (i * 8)) as u8)?;
        } else {
            *value |= u64::from(read(io, addr)?) << (i * 8);
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Mock {
        clock: u64,
        busy: bool,
        writes: alloc::vec::Vec<u8>,
    }
    impl Io for Mock {
        fn status(&mut self) -> Result<u8, Status> {
            Ok(if self.busy { 2 } else { 0x21 })
        }
        fn read_data(&mut self) -> Result<u8, Status> {
            Ok(0x42)
        }
        fn command(&mut self, v: u8) -> Result<(), Status> {
            self.writes.push(v);
            Ok(())
        }
        fn write_data(&mut self, v: u8) -> Result<(), Status> {
            self.writes.push(v);
            Ok(())
        }
        fn now_us(&self) -> u64 {
            self.clock
        }
        fn stall_us(&mut self, v: u32) {
            self.clock += u64::from(v);
        }
    }
    #[test]
    fn byte_protocol_and_bounds() {
        let mut m = Mock {
            clock: 0,
            busy: false,
            writes: alloc::vec::Vec::new(),
        };
        assert_eq!(read(&mut m, 5), Ok(0x42));
        assert_eq!(m.writes, [0x80, 5]);
        m.writes.clear();
        write(&mut m, 6, 7).unwrap();
        assert_eq!(m.writes, [0x81, 6, 7]);
        assert_eq!(query(&mut m), Ok(Some(0x42)));
        let mut v = 0;
        assert_eq!(transfer(&mut m, false, 255, 16, &mut v), Err(BAD_PARAMETER));
    }
    #[test]
    fn bad_controller_times_out() {
        let mut m = Mock {
            clock: 0,
            busy: true,
            writes: alloc::vec::Vec::new(),
        };
        assert_eq!(read(&mut m, 0), Err(TIME));
        assert!(m.writes.is_empty());
        assert_eq!(m.clock, 100_000);
    }
}

/// ECDT's validated SystemIO bootstrap resources. The ID is an absolute AML
/// path, never logged; UID is retained for matching/diagnostics by the owner.
#[derive(Debug, PartialEq, Eq)]
pub struct BootController {
    pub command: u16,
    pub data: u16,
    pub gpe: u8,
    pub uid: u32,
    pub path: alloc::string::String,
}
pub fn ecdt(table: &[u8]) -> Result<BootController, Status> {
    if table.len() < 66
        || &table[..4] != b"ECDT"
        || u32::from_le_bytes(table[4..8].try_into().unwrap()) as usize != table.len()
        || table.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) != 0
    {
        return Err(BAD_PARAMETER);
    }
    fn port(gas: &[u8]) -> Result<u16, Status> {
        let address = u64::from_le_bytes(gas[4..12].try_into().unwrap());
        if gas[0] != 1
            || gas[1] != 8
            || gas[2] != 0
            || gas[3] > 1
            || address == 0
            || address > u16::MAX.into()
        {
            return Err(crate::SUPPORT);
        }
        Ok(address as u16)
    }
    let name = &table[65..];
    let end = name.iter().position(|v| *v == 0).ok_or(BAD_PARAMETER)?;
    let path = core::str::from_utf8(&name[..end]).map_err(|_| BAD_PARAMETER)?;
    if !path.starts_with('\\') || path.len() > 4096 {
        return Err(BAD_PARAMETER);
    }
    Ok(BootController {
        command: port(&table[36..48])?,
        data: port(&table[48..60])?,
        uid: u32::from_le_bytes(table[60..64].try_into().unwrap()),
        gpe: table[64],
        path: path.into(),
    })
}
#[cfg(test)]
mod ecdt_tests {
    use super::*;
    #[test]
    fn table_validation() {
        let mut t = alloc::vec![0u8; 70];
        t[..4].copy_from_slice(b"ECDT");
        t[4..8].copy_from_slice(&70u32.to_le_bytes());
        for (offset, port) in [(36, 0x66u64), (48, 0x62)] {
            t[offset] = 1;
            t[offset + 1] = 8;
            t[offset + 4..offset + 12].copy_from_slice(&port.to_le_bytes());
        }
        t[64] = 9;
        t[65..].copy_from_slice(b"\\EC0\0");
        t[9] = 0u8.wrapping_sub(t.iter().fold(0u8, |a, b| a.wrapping_add(*b)));
        let ec = ecdt(&t).unwrap();
        assert_eq!((ec.command, ec.data, ec.gpe), (0x66, 0x62, 9));
        assert_eq!(ec.path, "\\EC0");
        t[9] ^= 1;
        assert_eq!(ecdt(&t), Err(BAD_PARAMETER));
        assert_eq!(ecdt(&t[..40]), Err(BAD_PARAMETER));
    }
}

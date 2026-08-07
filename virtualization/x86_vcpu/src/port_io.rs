//! State transition for one emulated x86 string-I/O element.

use crate::*;

/// Direction of a string port-I/O instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X86PortIoDirection {
    /// Read from the I/O port and store into guest memory (`INS`).
    In,
    /// Read guest memory and write it to the I/O port (`OUTS`).
    Out,
}

/// One element of a trapped `(REP) INS/OUTS` instruction.
///
/// The backend does not commit register or instruction-pointer changes until
/// the embedding VMM reports that both the device and guest-memory access
/// succeeded. Re-entering the guest then naturally retries the next element.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct X86PortIoStringExit {
    port: X86Port,
    width: X86AccessWidth,
    direction: X86PortIoDirection,
    guest_paddr: X86GuestPhysAddr,
    updated_index: u64,
    updated_count: Option<u64>,
    instruction_complete: bool,
    next_rip: u64,
}

pub(crate) struct X86PortIoAccess {
    pub(crate) port: X86Port,
    pub(crate) width: X86AccessWidth,
    pub(crate) direction: X86PortIoDirection,
    pub(crate) guest_paddr: X86GuestPhysAddr,
}

pub(crate) struct X86PortIoIteration {
    pub(crate) address_size: X86AddressSize,
    pub(crate) index: u64,
    pub(crate) count: u64,
    pub(crate) repeat: bool,
    pub(crate) decrement: bool,
    pub(crate) next_rip: u64,
}

impl X86PortIoStringExit {
    pub(crate) fn new(access: X86PortIoAccess, iteration: X86PortIoIteration) -> Self {
        let updated_index = iteration.address_size.offset(
            iteration.index,
            access.width.size() as u64,
            iteration.decrement,
        );
        let updated_count = iteration
            .repeat
            .then(|| iteration.address_size.decrement(iteration.count));
        let instruction_complete =
            updated_count.is_none_or(|count| iteration.address_size.low(count) == 0);
        Self {
            port: access.port,
            width: access.width,
            direction: access.direction,
            guest_paddr: access.guest_paddr,
            updated_index,
            updated_count,
            instruction_complete,
            next_rip: iteration.next_rip,
        }
    }

    /// Returns the I/O port.
    pub const fn port(self) -> X86Port {
        self.port
    }

    /// Returns the size of one transferred element.
    pub const fn width(self) -> X86AccessWidth {
        self.width
    }

    /// Returns the transfer direction.
    pub const fn direction(self) -> X86PortIoDirection {
        self.direction
    }

    /// Returns the translated guest physical address for this element.
    pub const fn guest_paddr(self) -> X86GuestPhysAddr {
        self.guest_paddr
    }

    pub(crate) const fn updated_index(self) -> u64 {
        self.updated_index
    }

    pub(crate) const fn updated_count(self) -> Option<u64> {
        self.updated_count
    }

    pub(crate) const fn instruction_complete(self) -> bool {
        self.instruction_complete
    }

    pub(crate) const fn next_rip(self) -> u64 {
        self.next_rip
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum X86AddressSize {
    Bits16,
    Bits32,
    Bits64,
}

impl X86AddressSize {
    pub(crate) const fn from_bytes(bytes: usize) -> X86VcpuResult<Self> {
        match bytes {
            2 => Ok(Self::Bits16),
            4 => Ok(Self::Bits32),
            8 => Ok(Self::Bits64),
            _ => Err(X86VcpuError::InvalidData),
        }
    }

    pub(crate) const fn low(self, value: u64) -> u64 {
        value & self.mask()
    }

    const fn decrement(self, value: u64) -> u64 {
        self.replace_low(value, self.low(value).wrapping_sub(1) & self.mask())
    }

    const fn offset(self, value: u64, delta: u64, decrement: bool) -> u64 {
        let low = if decrement {
            self.low(value).wrapping_sub(delta)
        } else {
            self.low(value).wrapping_add(delta)
        } & self.mask();
        self.replace_low(value, low)
    }

    const fn replace_low(self, original: u64, low: u64) -> u64 {
        match self {
            Self::Bits16 => (original & !0xffff) | low,
            Self::Bits32 | Self::Bits64 => low,
        }
    }

    const fn mask(self) -> u64 {
        match self {
            Self::Bits16 => 0xffff,
            Self::Bits32 => 0xffff_ffff,
            Self::Bits64 => u64::MAX,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_string_io_advances_only_after_the_last_element() {
        let first = X86PortIoStringExit::new(
            X86PortIoAccess {
                port: X86Port::new(0x511),
                width: X86AccessWidth::Byte,
                direction: X86PortIoDirection::In,
                guest_paddr: X86GuestPhysAddr::from_usize(0x2000),
            },
            X86PortIoIteration {
                address_size: X86AddressSize::Bits64,
                index: 0x2000,
                count: 2,
                repeat: true,
                decrement: false,
                next_rip: 0x102,
            },
        );
        assert_eq!(first.updated_index(), 0x2001);
        assert_eq!(first.updated_count(), Some(1));
        assert!(!first.instruction_complete());

        let last = X86PortIoStringExit::new(
            X86PortIoAccess {
                port: X86Port::new(0x511),
                width: X86AccessWidth::Byte,
                direction: X86PortIoDirection::In,
                guest_paddr: X86GuestPhysAddr::from_usize(0x2001),
            },
            X86PortIoIteration {
                address_size: X86AddressSize::Bits64,
                index: 0x2001,
                count: 1,
                repeat: true,
                decrement: false,
                next_rip: 0x102,
            },
        );
        assert_eq!(last.updated_index(), 0x2002);
        assert_eq!(last.updated_count(), Some(0));
        assert!(last.instruction_complete());
        assert_eq!(last.next_rip(), 0x102);
    }
}

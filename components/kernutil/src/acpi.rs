//! Readers for boot firmware tables.

use acpi::{
    AcpiError, AcpiTables, Handler, PhysicalMapping,
    address::{AddressSpace, GenericAddress, RawGenericAddress},
    sdt::{
        Signature,
        spcr::{SpcrInterfaceType, SpcrInterruptType},
    },
};

const LEGACY_SPCR_LEN: usize = 80;
const NAMESPACE_SPCR_LEN: usize = 88;
const SPCR_REVISION_WITH_CLOCK: u8 = 3;
const SPCR_REVISION_WITH_NAMESPACE: u8 = 4;

/// A mapping that covers exactly the length declared by an SPCR table.
pub struct SpcrMapping<H: Handler>(PhysicalMapping<H, u8>);

impl<H: Handler> SpcrMapping<H> {
    /// Reads an SPCR using the lifetime and length of this mapping.
    pub fn parse(&self) -> Option<Spcr<'_>> {
        let raw = self.0.raw();
        // SAFETY: the mapping owns a read-only, initialized firmware byte
        // region of region_length bytes, which remains live for this borrow.
        let bytes = unsafe { core::slice::from_raw_parts(self.0.as_ptr(), raw.region_length()) };
        Spcr::from_bytes(bytes)
    }
}

/// Maps SPCR entries using their header lengths, including legacy short tables.
///
/// # Safety
///
/// The handler must use the same physical address space as `tables`.
/// Their ACPI entries must reference initialized firmware memory spanning
/// each declared table length. Mappings must keep those bytes immutable and
/// readable until the matching unmap.
pub unsafe fn spcr_mappings<H: Handler>(
    tables: &AcpiTables<H>,
    handler: H,
) -> impl Iterator<Item = SpcrMapping<H>> {
    tables.table_headers().filter_map(move |(address, header)| {
        let length = header.length() as usize;
        if header.signature != Signature::SPCR
            || !(LEGACY_SPCR_LEN..=isize::MAX as usize).contains(&length)
        {
            return None;
        }
        // SAFETY: the caller guarantees the declared physical range. A byte
        // has alignment one and requires no newer revision's fields to exist.
        let mapping = unsafe { PhysicalMapping::new(address, length, handler.clone()) };
        Some(SpcrMapping(mapping))
    })
}

/// An invalid SPCR namespace string extent or encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidSpcrNamespace;

/// An SPCR table in its original byte representation.
pub struct Spcr<'a> {
    bytes: &'a [u8],
}

impl<'a> Spcr<'a> {
    /// Returns a view of a complete SPCR, bounded by its declared length.
    pub fn from_bytes(bytes: &'a [u8]) -> Option<Self> {
        if !bytes.starts_with(b"SPCR") {
            return None;
        }
        let length = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?) as usize;
        let revision = *bytes.get(8)?;
        let minimum = if revision >= SPCR_REVISION_WITH_NAMESPACE {
            NAMESPACE_SPCR_LEN
        } else {
            LEGACY_SPCR_LEN
        };
        if length < minimum {
            return None;
        }
        Some(Self {
            bytes: bytes.get(..length)?,
        })
    }

    /// UART model declared by the firmware.
    pub fn interface_type(&self) -> SpcrInterfaceType {
        self.bytes[36].into()
    }

    /// Register address and access parameters, if console redirection is enabled.
    pub fn base_address(&self) -> Option<Result<GenericAddress, AcpiError>> {
        let gas = &self.bytes[40..52];
        let address = u64::from_le_bytes(gas[4..].try_into().unwrap());
        if address == 0 {
            return None;
        }
        Some(GenericAddress::from_raw(RawGenericAddress {
            address_space: gas[0],
            bit_width: gas[1],
            bit_offset: gas[2],
            access_size: gas[3],
            address,
        }))
    }

    /// Decodes the NS16550 MMIO register stride and access width in bytes.
    ///
    /// In the Generic 16550 subtype, GAS Register Bit Width encodes the
    /// spacing independently of Access Size. Keep the access width as the
    /// spacing for legacy descriptors that leave the stride unspecified.
    pub fn ns16550_mmio_layout(&self) -> Option<(usize, usize)> {
        let gas = self.base_address()?.ok()?;
        if gas.address_space != AddressSpace::SystemMemory || gas.bit_offset != 0 {
            return None;
        }
        let width = match (gas.access_size, gas.bit_width) {
            (0, 0 | 8) | (1, _) => 1,
            (0, 16) | (2, _) => 2,
            (0, 32) | (3, _) => 4,
            _ => return None,
        };
        let stride = match self.interface_type() {
            SpcrInterfaceType::Full16550 => width,
            SpcrInterfaceType::Generic16550 => match gas.bit_width {
                0 => width,
                8 => 1,
                16 => 2,
                32 => 4,
                64 => 8,
                _ => return None,
            },
            _ => return None,
        };
        (stride >= width).then_some((stride, width))
    }

    /// Global system interrupt, or the legacy PC-AT IRQ for an 8259-only port.
    pub fn irq(&self) -> Option<u32> {
        let types = SpcrInterruptType::from_bits_truncate(self.bytes[52]);
        let legacy = SpcrInterruptType::DUAL_8259;
        if !types.difference(legacy).is_empty() {
            Some(self.read_u32(54))
        } else {
            types.contains(legacy).then(|| u32::from(self.bytes[53]))
        }
    }

    /// UART input clock in Hz when firmware supplies it.
    pub fn uart_clock_frequency(&self) -> Option<u32> {
        if self.bytes[8] < SPCR_REVISION_WITH_CLOCK {
            return None;
        }
        let clock = self.read_u32(76);
        (clock != 0).then_some(clock)
    }

    /// The configured baud rate, preferring the revision 4 precise value.
    pub fn baud_rate(&self) -> Option<u32> {
        if self.bytes[8] >= SPCR_REVISION_WITH_NAMESPACE {
            let precise = self.read_u32(80);
            if precise != 0 {
                return Some(precise);
            }
        }
        match self.bytes[58] {
            3 => Some(9_600),
            4 => Some(19_200),
            6 => Some(57_600),
            7 => Some(115_200),
            _ => None,
        }
    }

    /// The namespace string without trailing NULs. A dot denotes no object.
    pub fn namespace_path(&self) -> Result<Option<&'a str>, InvalidSpcrNamespace> {
        if self.bytes[8] < SPCR_REVISION_WITH_NAMESPACE {
            return Ok(None);
        }
        let length = usize::from(u16::from_le_bytes(self.bytes[84..86].try_into().unwrap()));
        let offset = usize::from(u16::from_le_bytes(self.bytes[86..88].try_into().unwrap()));
        if offset < NAMESPACE_SPCR_LEN {
            return Err(InvalidSpcrNamespace);
        }
        let bytes = self
            .bytes
            .get(offset..offset + length)
            .ok_or(InvalidSpcrNamespace)?;
        let name = bytes.strip_suffix(&[0]).ok_or(InvalidSpcrNamespace)?;
        let name = core::str::from_utf8(name)
            .map_err(|_| InvalidSpcrNamespace)?
            .trim_end_matches('\0');
        Ok((!name.is_empty() && name != ".").then_some(name))
    }

    fn read_u32(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.bytes[offset..offset + 4].try_into().unwrap())
    }
}

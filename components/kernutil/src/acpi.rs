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
// Both the namespace offset and length are u16. No supported field can
// reference bytes beyond their largest combined extent.
const MAX_SPCR_LEN: usize = 2 * u16::MAX as usize;

/// A validated SPCR mapping, or a retained invalid entry with no table mapping.
pub struct SpcrMapping<H: Handler> {
    mapping: Option<PhysicalMapping<H, u8>>,
    length: usize,
}

impl<H: Handler> SpcrMapping<H> {
    /// Reads an SPCR using the lifetime and length of this mapping.
    pub fn parse(&self) -> Option<Spcr<'_>> {
        let mapping = self.mapping.as_ref()?;
        // SAFETY: spcr_mappings checked this exact extent with the firmware
        // memory owner before mapping it. The mapping retains those immutable
        // bytes for this borrow; the checked length fits the address space.
        let bytes = unsafe { core::slice::from_raw_parts(mapping.as_ptr(), self.length) };
        Spcr::from_bytes(bytes)
    }
}

/// Maps SPCR entries only after the memory owner validates their entire extent.
/// Invalid entries are retained and return `None` from `SpcrMapping::parse`.
///
/// # Safety
///
/// The handler must use the same physical address space as `tables`.
/// Their ACPI entries must reference initialized firmware table headers.
/// `contains` must accept only initialized, immutable firmware ranges that
/// the handler can map in full and retain until the matching unmap. It must
/// check actual backing, independently of the table's declared length.
pub unsafe fn spcr_mappings<H: Handler>(
    tables: &AcpiTables<H>,
    handler: H,
    contains: impl Fn(usize, usize) -> bool,
) -> impl Iterator<Item = SpcrMapping<H>> {
    tables.table_headers().filter_map(move |(address, header)| {
        if header.signature != Signature::SPCR {
            return None;
        }
        let length = header.length() as usize;
        let minimum = if header.revision >= SPCR_REVISION_WITH_NAMESPACE {
            NAMESPACE_SPCR_LEN
        } else {
            LEGACY_SPCR_LEN
        };
        let mapping = if (minimum..=MAX_SPCR_LEN.min(isize::MAX as usize)).contains(&length)
            && address.checked_add(length).is_some()
            && contains(address, length)
        {
            // SAFETY: the memory owner accepted the entire bounded extent.
            let mapping =
                unsafe { PhysicalMapping::<_, u8>::new(address, length, handler.clone()) };
            // A physical-to-virtual translation must not wrap the slice either.
            unsafe { mapping.as_ptr() }
                .addr()
                .checked_add(length)
                .map(|_| mapping)
        } else {
            None
        };
        Some(SpcrMapping { mapping, length })
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

    /// Decodes a 16550-compatible UART with eight consecutive byte I/O ports.
    pub fn ns16550_io_port(&self) -> Option<u16> {
        let gas = self.base_address()?.ok()?;
        if !matches!(
            self.interface_type(),
            SpcrInterfaceType::Full16550
                | SpcrInterfaceType::Full16450
                | SpcrInterfaceType::Generic16550
        ) || gas.address_space != AddressSpace::SystemIo
            || gas.bit_offset != 0
            || !matches!(gas.bit_width, 0 | 8)
            || !matches!(gas.access_size, 0 | 1)
        {
            return None;
        }
        let port = u16::try_from(gas.address).ok()?;
        port.checked_add(7)?;
        Some(port)
    }

    /// Decodes the NS16550 MMIO register stride and access width in bytes.
    ///
    /// In the Generic 16550 subtype, GAS Register Bit Width encodes the
    /// spacing independently of Access Size. Full 16550 and Full 16450 use
    /// the access width as the spacing.
    pub fn ns16550_mmio_layout(&self) -> Option<(usize, usize)> {
        let gas = self.base_address()?.ok()?;
        if gas.address_space != AddressSpace::SystemMemory || gas.bit_offset != 0 {
            return None;
        }
        let width = match (gas.access_size, gas.bit_width) {
            (0, 0 | 8) | (1, _) => 1,
            (0, 16) | (2, _) => 2,
            (0, 32) | (3, _) => 4,
            (0, 64) | (4, _) if usize::BITS == 64 => 8,
            _ => return None,
        };
        let stride = match self.interface_type() {
            SpcrInterfaceType::Full16550 | SpcrInterfaceType::Full16450 => width,
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

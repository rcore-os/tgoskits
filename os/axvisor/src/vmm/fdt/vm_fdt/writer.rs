use alloc::collections::BTreeMap;
use alloc::ffi::CString;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::{Ord, Ordering};
use core::convert::TryInto;
use core::fmt;
use core::mem::size_of_val;
use hashbrown::HashSet;

use super::{
    FDT_BEGIN_NODE, FDT_END, FDT_END_NODE, FDT_MAGIC, FDT_PROP, NODE_NAME_MAX_LEN,
    PROPERTY_NAME_MAX_LEN,
};

#[derive(Debug, Eq, PartialEq)]
/// Errors associated with creating the Flattened Device Tree.
pub enum Error {
    /// Properties may not be added before beginning a node.
    PropertyBeforeBeginNode,
    /// Properties may not be added after a node has been ended.
    PropertyAfterEndNode,
    /// Property value size must fit in 32 bits.
    PropertyValueTooLarge,
    /// Total size must fit in 32 bits.
    TotalSizeTooLarge,
    /// Strings cannot contain NUL.
    InvalidString,
    /// Attempted to end a node that was not the most recent.
    OutOfOrderEndNode,
    /// Attempted to call finish without ending all nodes.
    UnclosedNode,
    /// Memory reservation is invalid.
    InvalidMemoryReservation,
    /// Memory reservations are overlapping.
    OverlappingMemoryReservations,
    /// Invalid node name.
    InvalidNodeName,
    /// Invalid property name.
    InvalidPropertyName,
    /// Node depth exceeds FDT_MAX_NODE_DEPTH
    NodeDepthTooLarge,
    /// Duplicate phandle property
    DuplicatePhandle,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::PropertyBeforeBeginNode => {
                write!(f, "Properties may not be added before beginning a node")
            }
            Error::PropertyAfterEndNode => {
                write!(f, "Properties may not be added after a node has been ended")
            }
            Error::PropertyValueTooLarge => write!(f, "Property value size must fit in 32 bits"),
            Error::TotalSizeTooLarge => write!(f, "Total size must fit in 32 bits"),
            Error::InvalidString => write!(f, "Strings cannot contain NUL"),
            Error::OutOfOrderEndNode => {
                write!(f, "Attempted to end a node that was not the most recent")
            }
            Error::UnclosedNode => write!(f, "Attempted to call finish without ending all nodes"),
            Error::InvalidMemoryReservation => write!(f, "Memory reservation is invalid"),
            Error::OverlappingMemoryReservations => {
                write!(f, "Memory reservations are overlapping")
            }
            Error::InvalidNodeName => write!(f, "Invalid node name"),
            Error::InvalidPropertyName => write!(f, "Invalid property name"),
            Error::NodeDepthTooLarge => write!(f, "Node depth exceeds FDT_MAX_NODE_DEPTH"),
            Error::DuplicatePhandle => write!(f, "Duplicate phandle value"),
        }
    }
}

/// Result of a FDT writer operation.
pub type Result<T> = core::result::Result<T, Error>;

const FDT_HEADER_SIZE: usize = 40;
const FDT_VERSION: u32 = 17;
const FDT_LAST_COMP_VERSION: u32 = 16;
/// The same max depth as in the Linux kernel.
const FDT_MAX_NODE_DEPTH: usize = 64;

/// Interface for writing a Flattened Devicetree (FDT) and emitting a Devicetree Blob (DTB).
#[derive(Debug)]
pub struct FdtWriter {
    data: Vec<u8>,
    off_mem_rsvmap: u32,
    off_dt_struct: u32,
    strings: Vec<u8>,
    string_offsets: BTreeMap<CString, u32>,
    node_depth: usize,
    node_ended: bool,
    boot_cpuid_phys: u32,
    // The set is used to track the uniqueness of phandle values as required by the spec
    // https://devicetree-specification.readthedocs.io/en/stable/devicetree-basics.html#phandle
    #[allow(dead_code)]
    phandles: HashSet<u32>,
}

/// Reserved physical memory region.
///
/// This represents an area of physical memory reserved by the firmware and unusable by the OS.
/// For example, this could be used to preserve bootloader code or data used at runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FdtReserveEntry {
    address: u64,
    size: u64,
}

impl FdtReserveEntry {
    /// Create a memory reservation for the FDT.
    ///
    /// # Arguments
    ///
    /// * address: Physical address of the beginning of the reserved region.
    /// * size: Size of the reserved region in bytes.
    #[allow(dead_code)]
    pub fn new(address: u64, size: u64) -> Result<Self> {
        if address.checked_add(size).is_none() || size == 0 {
            return Err(Error::InvalidMemoryReservation);
        }

        Ok(FdtReserveEntry { address, size })
    }
}

impl Ord for FdtReserveEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.address.cmp(&other.address)
    }
}

impl PartialOrd for FdtReserveEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// Returns true if there are any overlapping memory reservations.
fn check_overlapping(mem_reservations: &[FdtReserveEntry]) -> Result<()> {
    let mut mem_rsvmap_copy = mem_reservations.to_vec();
    mem_rsvmap_copy.sort();
    let overlapping = mem_rsvmap_copy.windows(2).any(|w| {
        // The following add cannot overflow because we can only have
        // valid FdtReserveEntry (as per the constructor of the type).
        w[0].address + w[0].size > w[1].address
    });

    if overlapping {
        return Err(Error::OverlappingMemoryReservations);
    }

    Ok(())
}

// Check if `name` is a valid node name in the form "node-name@unit-address".
// https://devicetree-specification.readthedocs.io/en/stable/devicetree-basics.html#node-name-requirements
fn node_name_valid(name: &str) -> bool {
    // Special case: allow empty node names.
    // This is technically not allowed by the spec, but it seems to be accepted in practice.
    if name.is_empty() {
        return true;
    }

    let mut parts = name.split('@');

    let node_name = parts.next().unwrap(); // split() always returns at least one part
    let unit_address = parts.next();

    if unit_address.is_some() && parts.next().is_some() {
        // Node names should only contain one '@'.
        return false;
    }

    if node_name.is_empty() || node_name.len() > NODE_NAME_MAX_LEN {
        return false;
    }

    // if !node_name.starts_with(node_name_valid_first_char) {
    //     return false;
    // }

    if node_name.contains(|c: char| !node_name_valid_char(c)) {
        return false;
    }

    if let Some(unit_address) = unit_address
        && unit_address.contains(|c: char| !node_name_valid_char(c))
    {
        return false;
    }

    true
}

fn node_name_valid_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ',' | '.' | '_' | '+' | '-')
}

#[allow(dead_code)]
fn node_name_valid_first_char(c: char) -> bool {
    c.is_ascii_alphabetic()
}

// Check if `name` is a valid property name.
// https://devicetree-specification.readthedocs.io/en/stable/devicetree-basics.html#property-names
fn property_name_valid(name: &str) -> bool {
    if name.is_empty() || name.len() > PROPERTY_NAME_MAX_LEN {
        return false;
    }

    if name.contains(|c: char| !property_name_valid_char(c)) {
        return false;
    }

    true
}

fn property_name_valid_char(c: char) -> bool {
    matches!(c, '0'..='9' | 'a'..='z' | 'A'..='Z' | ',' | '.' | '_' | '+' | '?' | '#' | '-')
}

/// Handle to an open node created by `FdtWriter::begin_node`.
///
/// This must be passed back to `FdtWriter::end_node` to close the nodes.
/// Nodes must be closed in reverse order as they were opened, matching the nesting structure
/// of the devicetree.
#[derive(Debug)]
pub struct FdtWriterNode {
    depth: usize,
}

impl FdtWriter {
    /// Create a new Flattened Devicetree writer instance.
    pub fn new() -> Result<Self> {
        FdtWriter::new_with_mem_reserv(&[])
    }

    /// Create a new Flattened Devicetree writer instance.
    ///
    /// # Arguments
    ///
    /// `mem_reservations` - reserved physical memory regions to list in the FDT header.
    pub fn new_with_mem_reserv(mem_reservations: &[FdtReserveEntry]) -> Result<Self> {
        let data = vec![0u8; FDT_HEADER_SIZE]; // Reserve space for header.

        let mut fdt = FdtWriter {
            data,
            off_mem_rsvmap: 0,
            off_dt_struct: 0,
            strings: Vec::new(),
            string_offsets: BTreeMap::new(),
            node_depth: 0,
            node_ended: false,
            boot_cpuid_phys: 0,
            phandles: HashSet::new(),
        };

        fdt.align(8);
        // This conversion cannot fail since the size of the header is fixed.
        fdt.off_mem_rsvmap = fdt.data.len() as u32;

        check_overlapping(mem_reservations)?;
        fdt.write_mem_rsvmap(mem_reservations);

        fdt.align(4);
        fdt.off_dt_struct = fdt
            .data
            .len()
            .try_into()
            .map_err(|_| Error::TotalSizeTooLarge)?;

        Ok(fdt)
    }

    fn write_mem_rsvmap(&mut self, mem_reservations: &[FdtReserveEntry]) {
        for rsv in mem_reservations {
            self.append_u64(rsv.address);
            self.append_u64(rsv.size);
        }

        self.append_u64(0);
        self.append_u64(0);
    }

    /// Set the `boot_cpuid_phys` field of the devicetree header.
    ///
    /// # Example
    ///
    /// ```rust
    /// use vm_fdt::{Error, FdtWriter};
    ///
    /// fn create_fdt() -> Result<Vec<u8>, Error> {
    ///     let mut fdt = FdtWriter::new()?;
    ///     fdt.set_boot_cpuid_phys(0x12345678);
    ///     // ... add other nodes & properties
    ///     fdt.finish()
    /// }
    ///
    /// # let dtb = create_fdt().unwrap();
    /// ```
    #[allow(dead_code)]
    pub fn set_boot_cpuid_phys(&mut self, boot_cpuid_phys: u32) {
        self.boot_cpuid_phys = boot_cpuid_phys;
    }

    // Append `num_bytes` padding bytes (0x00).
    fn pad(&mut self, num_bytes: usize) {
        self.data.extend(core::iter::repeat_n(0, num_bytes));
    }

    // Append padding bytes (0x00) until the length of data is a multiple of `alignment`.
    fn align(&mut self, alignment: usize) {
        let offset = self.data.len() % alignment;
        if offset != 0 {
            self.pad(alignment - offset);
        }
    }

    // Rewrite the value of a big-endian u32 within data.
    fn update_u32(&mut self, offset: usize, val: u32) {
        // Safe to use `+ 4` since we are calling this function with small values, and it's a
        // private function.
        let data_slice = &mut self.data[offset..offset + 4];
        data_slice.copy_from_slice(&val.to_be_bytes());
    }

    fn append_u32(&mut self, val: u32) {
        self.data.extend_from_slice(&val.to_be_bytes());
    }

    fn append_u64(&mut self, val: u64) {
        self.data.extend_from_slice(&val.to_be_bytes());
    }

    /// Open a new FDT node.
    ///
    /// The node must be closed using `end_node`.
    ///
    /// # Arguments
    ///
    /// `name` - name of the node; must not contain any NUL bytes.
    pub fn begin_node(&mut self, name: &str) -> Result<FdtWriterNode> {
        if self.node_depth >= FDT_MAX_NODE_DEPTH {
            return Err(Error::NodeDepthTooLarge);
        }

        let name_cstr = CString::new(name).map_err(|_| Error::InvalidString)?;
        // The unit adddress part of the node name, if present, is not fully validated
        // since the exact requirements depend on the bus mapping.
        // https://devicetree-specification.readthedocs.io/en/stable/devicetree-basics.html#node-name-requirements
        if !node_name_valid(name) {
            return Err(Error::InvalidNodeName);
        }
        self.append_u32(FDT_BEGIN_NODE);
        self.data.extend(name_cstr.to_bytes_with_nul());
        self.align(4);
        // This can not overflow due to the `if` at the beginning of the function
        // where the current depth is checked against FDT_MAX_NODE_DEPTH.
        self.node_depth += 1;
        self.node_ended = false;
        Ok(FdtWriterNode {
            depth: self.node_depth,
        })
    }

    /// Close a node previously opened with `begin_node`.
    pub fn end_node(&mut self, node: FdtWriterNode) -> Result<()> {
        if node.depth != self.node_depth {
            return Err(Error::OutOfOrderEndNode);
        }

        self.append_u32(FDT_END_NODE);
        // This can not underflow. The above `if` makes sure there is at least one open node
        // (node_depth >= 1).
        self.node_depth -= 1;
        self.node_ended = true;
        Ok(())
    }

    // Find an existing instance of a string `s`, or add it to the strings block.
    // Returns the offset into the strings block.
    fn intern_string(&mut self, s: CString) -> Result<u32> {
        if let Some(off) = self.string_offsets.get(&s) {
            Ok(*off)
        } else {
            let off = self
                .strings
                .len()
                .try_into()
                .map_err(|_| Error::TotalSizeTooLarge)?;
            self.strings.extend_from_slice(s.to_bytes_with_nul());
            self.string_offsets.insert(s, off);
            Ok(off)
        }
    }

    /// Write a property.
    ///
    /// # Arguments
    ///
    /// `name` - name of the property; must not contain any NUL bytes.
    /// `val` - value of the property (raw byte array).
    pub fn property(&mut self, name: &str, val: &[u8]) -> Result<()> {
        if self.node_ended {
            return Err(Error::PropertyAfterEndNode);
        }

        if self.node_depth == 0 {
            return Err(Error::PropertyBeforeBeginNode);
        }

        let name_cstr = CString::new(name).map_err(|_| Error::InvalidString)?;

        if !property_name_valid(name) {
            return Err(Error::InvalidPropertyName);
        }

        let len = val
            .len()
            .try_into()
            .map_err(|_| Error::PropertyValueTooLarge)?;

        let nameoff = self.intern_string(name_cstr)?;
        self.append_u32(FDT_PROP);
        self.append_u32(len);
        self.append_u32(nameoff);
        self.data.extend_from_slice(val);
        self.align(4);
        Ok(())
    }

    /// Write an empty property.
    #[allow(dead_code)]
    pub fn property_null(&mut self, name: &str) -> Result<()> {
        self.property(name, &[])
    }

    /// Write a string property.
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64", test))]
    pub fn property_string(&mut self, name: &str, val: &str) -> Result<()> {
        let cstr_value = CString::new(val).map_err(|_| Error::InvalidString)?;
        self.property(name, cstr_value.to_bytes_with_nul())
    }

    /// Write a stringlist property.
    #[allow(dead_code)]
    pub fn property_string_list(&mut self, name: &str, values: Vec<String>) -> Result<()> {
        let mut bytes = Vec::new();
        for s in values {
            let cstr = CString::new(s).map_err(|_| Error::InvalidString)?;
            bytes.extend_from_slice(cstr.to_bytes_with_nul());
        }
        self.property(name, &bytes)
    }

    /// Write a 32-bit unsigned integer property.
    #[allow(dead_code)]
    pub fn property_u32(&mut self, name: &str, val: u32) -> Result<()> {
        self.property(name, &val.to_be_bytes())
    }

    /// Write a 64-bit unsigned integer property.
    #[allow(dead_code)]
    pub fn property_u64(&mut self, name: &str, val: u64) -> Result<()> {
        self.property(name, &val.to_be_bytes())
    }

    /// Write a property containing an array of 32-bit unsigned integers.
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64", test))]
    pub fn property_array_u32(&mut self, name: &str, cells: &[u32]) -> Result<()> {
        let mut arr = Vec::with_capacity(size_of_val(cells));
        for &c in cells {
            arr.extend(c.to_be_bytes());
        }
        self.property(name, &arr)
    }

    /// Write a property containing an array of 64-bit unsigned integers.
    #[allow(dead_code)]
    pub fn property_array_u64(&mut self, name: &str, cells: &[u64]) -> Result<()> {
        let mut arr = Vec::with_capacity(size_of_val(cells));
        for &c in cells {
            arr.extend(c.to_be_bytes());
        }
        self.property(name, &arr)
    }

    /// Write a [`phandle`](https://devicetree-specification.readthedocs.io/en/stable/devicetree-basics.html?#phandle)
    /// property. The value is checked for uniqueness within the FDT. In the case of a duplicate
    /// [`Error::DuplicatePhandle`] is returned.
    #[allow(dead_code)]
    pub fn property_phandle(&mut self, val: u32) -> Result<()> {
        if !self.phandles.insert(val) {
            return Err(Error::DuplicatePhandle);
        }
        self.property("phandle", &val.to_be_bytes())
    }

    /// Finish writing the Devicetree Blob (DTB).
    ///
    /// Returns the DTB as a vector of bytes, consuming the `FdtWriter`.
    pub fn finish(mut self) -> Result<Vec<u8>> {
        if self.node_depth > 0 {
            return Err(Error::UnclosedNode);
        }

        self.append_u32(FDT_END);
        let size_dt_plus_header: u32 = self
            .data
            .len()
            .try_into()
            .map_err(|_| Error::TotalSizeTooLarge)?;
        // The following operation cannot fail because the total size of data
        // also includes the offset, and we checked that `size_dt_plus_header`
        // does not wrap around when converted to an u32.
        let size_dt_struct = size_dt_plus_header - self.off_dt_struct;

        let off_dt_strings = self
            .data
            .len()
            .try_into()
            .map_err(|_| Error::TotalSizeTooLarge)?;
        let size_dt_strings = self
            .strings
            .len()
            .try_into()
            .map_err(|_| Error::TotalSizeTooLarge)?;

        let totalsize = self
            .data
            .len()
            .checked_add(self.strings.len())
            .ok_or(Error::TotalSizeTooLarge)?;
        let totalsize = totalsize.try_into().map_err(|_| Error::TotalSizeTooLarge)?;

        // Finalize the header.
        self.update_u32(0, FDT_MAGIC);
        self.update_u32(4, totalsize);
        self.update_u32(2 * 4, self.off_dt_struct);
        self.update_u32(3 * 4, off_dt_strings);
        self.update_u32(4 * 4, self.off_mem_rsvmap);
        self.update_u32(5 * 4, FDT_VERSION);
        self.update_u32(6 * 4, FDT_LAST_COMP_VERSION);
        self.update_u32(7 * 4, self.boot_cpuid_phys);
        self.update_u32(8 * 4, size_dt_strings);
        self.update_u32(9 * 4, size_dt_struct);

        // Add the strings block.
        self.data.append(&mut self.strings);

        Ok(self.data)
    }
}

//! Device tree property representation and manipulation.
//!
//! This module provides the `Property` type which represents a mutable device tree
//! property with a name and data, along with methods for accessing and modifying
//! various property data formats.

use core::ffi::CStr;

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use fdt_raw::data::{Bytes, StrIter, U32Iter};
// Re-export from fdt_raw
use crate::Reader;

/// A mutable device tree property.
///
/// Represents a property with a name and raw data. Provides methods for
/// accessing and modifying the data in various formats (u32, u64, strings, etc.).
#[derive(Clone)]
pub struct Property {
    /// Property name
    pub name: String,
    /// Raw property data
    pub data: Vec<u8>,
}

impl Property {
    /// Creates a new property with the given name and data.
    pub fn new(name: &str, data: Vec<u8>) -> Self {
        Self {
            name: name.to_string(),
            data,
        }
    }

    /// Returns the property name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the property data as a big-endian u32.
    ///
    /// Returns None if the data is not exactly 4 bytes.
    pub fn get_u32(&self) -> Option<u32> {
        if self.data.len() != 4 {
            return None;
        }
        Some(u32::from_be_bytes([
            self.data[0],
            self.data[1],
            self.data[2],
            self.data[3],
        ]))
    }

    /// Sets the property data from a list of u32 values (as big-endian).
    pub fn set_u32_ls(&mut self, values: &[u32]) {
        self.data.clear();
        for &value in values {
            self.data.extend_from_slice(&value.to_be_bytes());
        }
    }

    /// Returns an iterator over u32 values in the property data.
    pub fn get_u32_iter(&self) -> U32Iter<'_> {
        Bytes::new(&self.data).as_u32_iter()
    }

    /// Returns the property data as a big-endian u64.
    ///
    /// Returns None if the data is not exactly 8 bytes.
    pub fn get_u64(&self) -> Option<u64> {
        if self.data.len() != 8 {
            return None;
        }
        Some(u64::from_be_bytes([
            self.data[0],
            self.data[1],
            self.data[2],
            self.data[3],
            self.data[4],
            self.data[5],
            self.data[6],
            self.data[7],
        ]))
    }

    /// Sets the property data from a u64 value (as big-endian).
    pub fn set_u64(&mut self, value: u64) {
        self.data = value.to_be_bytes().to_vec();
    }

    /// Returns the property data as a null-terminated string.
    ///
    /// Returns None if the data is not a valid null-terminated UTF-8 string.
    pub fn as_str(&self) -> Option<&str> {
        CStr::from_bytes_with_nul(&self.data)
            .ok()
            .and_then(|cstr| cstr.to_str().ok())
    }

    /// Sets the property data from a string value.
    ///
    /// The string will be null-terminated.
    pub fn set_string(&mut self, value: &str) {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0); // Null-terminate
        self.data = bytes;
    }

    /// Returns an iterator over null-terminated strings in the property data.
    pub fn as_str_iter(&self) -> StrIter<'_> {
        Bytes::new(&self.data).as_str_iter()
    }

    /// Sets the property data from a list of string values.
    ///
    /// Each string will be null-terminated.
    pub fn set_string_ls(&mut self, values: &[&str]) {
        self.data.clear();
        for &value in values {
            self.data.extend_from_slice(value.as_bytes());
            self.data.push(0); // Null-terminate each string
        }
    }

    /// Returns a reader for accessing the property data.
    pub fn as_reader(&self) -> Reader<'_> {
        Bytes::new(&self.data).reader()
    }
}

impl From<&fdt_raw::Property<'_>> for Property {
    fn from(value: &fdt_raw::Property<'_>) -> Self {
        Self {
            name: value.name().to_string(),
            data: value.as_slice().to_vec(),
        }
    }
}

/// Ranges entry information for address translation.
///
/// Represents a single entry in a `ranges` property, mapping a child bus
/// address range to a parent bus address range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangesEntry {
    /// Child bus address
    pub child_bus_address: u64,
    /// Parent bus address
    pub parent_bus_address: u64,
    /// Length of the region
    pub length: u64,
}

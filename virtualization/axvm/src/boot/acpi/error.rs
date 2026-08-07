//! ACPI composition failures.

use std::string::String;

/// Error returned while laying out or serializing guest ACPI tables.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AcpiBuildError {
    /// A requested alignment is not valid.
    #[error("ACPI object {object} has invalid alignment {alignment:#x}")]
    InvalidAlignment {
        /// Logical table or file name.
        object: String,
        /// Requested alignment.
        alignment: usize,
    },
    /// Address arithmetic overflowed.
    #[error("ACPI object {object} address range overflows")]
    AddressOverflow {
        /// Logical table or file name.
        object: String,
    },
    /// The configured arena cannot hold another table.
    #[error(
        "ACPI arena [{base:#x}, {limit:#x}) cannot fit {object} ({size:#x} bytes, align \
         {alignment:#x})"
    )]
    ArenaExhausted {
        /// Arena base GPA.
        base: u64,
        /// Exclusive arena limit.
        limit: u64,
        /// Logical table name.
        object: String,
        /// Requested size.
        size: usize,
        /// Requested alignment.
        alignment: usize,
    },
    /// A write does not match a reservation.
    #[error("ACPI object {object} write length {actual:#x} differs from reservation {expected:#x}")]
    LengthMismatch {
        /// Logical table name.
        object: String,
        /// Reserved length.
        expected: usize,
        /// Supplied bytes.
        actual: usize,
    },
    /// A duplicate logical table was registered.
    #[cfg(any(target_arch = "x86_64", test))]
    #[error("ACPI table {signature:?} is registered more than once")]
    DuplicateTable {
        /// Duplicate ACPI signature.
        signature: [u8; 4],
    },
    /// A numeric architecture value cannot be represented by the table format.
    #[error("ACPI value {value} is invalid for {field}")]
    InvalidValue {
        /// Field being encoded.
        field: &'static str,
        /// Rendered value.
        value: String,
    },
    /// A QEMU table-loader file name is invalid.
    #[error("ACPI loader file name '{name}' is empty or exceeds 55 bytes")]
    InvalidLoaderFile {
        /// Invalid file name.
        name: String,
    },
}

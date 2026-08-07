//! QEMU-compatible ACPI table-loader command generation.

use std::{string::String, vec::Vec};

use super::AcpiBuildError;

const COMMAND_ALLOCATE: u32 = 1;
const COMMAND_ADD_POINTER: u32 = 2;
const COMMAND_ADD_CHECKSUM: u32 = 3;
const ENTRY_SIZE: usize = 128;

/// Firmware allocation zone from the QEMU table-loader ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LoaderZone {
    High = 1,
    Fseg = 2,
}

enum LoaderCommand {
    Allocate {
        file: String,
        alignment: u32,
        zone: LoaderZone,
    },
    AddPointer {
        pointer_file: String,
        pointee_file: String,
        pointer_offset: u32,
        pointer_size: u8,
    },
    AddChecksum {
        file: String,
        checksum_offset: u32,
        start: u32,
        length: u32,
    },
}

/// Ordered relocation and checksum operations for fw_cfg ACPI files.
#[derive(Default)]
pub(crate) struct AcpiLoaderPlan {
    commands: Vec<LoaderCommand>,
}

impl AcpiLoaderPlan {
    pub(crate) const fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    pub(crate) fn allocate(
        &mut self,
        file: &str,
        alignment: u32,
        zone: LoaderZone,
    ) -> Result<(), AcpiBuildError> {
        validate_file(file)?;
        if !alignment.is_power_of_two() {
            return Err(AcpiBuildError::InvalidAlignment {
                object: file.into(),
                alignment: alignment as usize,
            });
        }
        self.commands.push(LoaderCommand::Allocate {
            file: file.into(),
            alignment,
            zone,
        });
        Ok(())
    }

    pub(crate) fn add_pointer(
        &mut self,
        pointer_file: &str,
        pointee_file: &str,
        pointer_offset: u32,
        pointer_size: u8,
    ) -> Result<(), AcpiBuildError> {
        validate_file(pointer_file)?;
        validate_file(pointee_file)?;
        if !matches!(pointer_size, 1 | 2 | 4 | 8) {
            return Err(AcpiBuildError::InvalidValue {
                field: "table-loader pointer size",
                value: std::format!("{pointer_size}"),
            });
        }
        self.commands.push(LoaderCommand::AddPointer {
            pointer_file: pointer_file.into(),
            pointee_file: pointee_file.into(),
            pointer_offset,
            pointer_size,
        });
        Ok(())
    }

    pub(crate) fn add_checksum(
        &mut self,
        file: &str,
        checksum_offset: u32,
        start: u32,
        length: u32,
    ) -> Result<(), AcpiBuildError> {
        validate_file(file)?;
        self.commands.push(LoaderCommand::AddChecksum {
            file: file.into(),
            checksum_offset,
            start,
            length,
        });
        Ok(())
    }

    pub(crate) fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.commands.len() * ENTRY_SIZE);
        for command in &self.commands {
            let mut entry = [0u8; ENTRY_SIZE];
            match command {
                LoaderCommand::Allocate {
                    file,
                    alignment,
                    zone,
                } => {
                    entry[0..4].copy_from_slice(&COMMAND_ALLOCATE.to_le_bytes());
                    write_file(&mut entry[4..60], file);
                    entry[60..64].copy_from_slice(&alignment.to_le_bytes());
                    entry[64] = *zone as u8;
                }
                LoaderCommand::AddPointer {
                    pointer_file,
                    pointee_file,
                    pointer_offset,
                    pointer_size,
                } => {
                    entry[0..4].copy_from_slice(&COMMAND_ADD_POINTER.to_le_bytes());
                    write_file(&mut entry[4..60], pointer_file);
                    write_file(&mut entry[60..116], pointee_file);
                    entry[116..120].copy_from_slice(&pointer_offset.to_le_bytes());
                    entry[120] = *pointer_size;
                }
                LoaderCommand::AddChecksum {
                    file,
                    checksum_offset,
                    start,
                    length,
                } => {
                    entry[0..4].copy_from_slice(&COMMAND_ADD_CHECKSUM.to_le_bytes());
                    write_file(&mut entry[4..60], file);
                    entry[60..64].copy_from_slice(&checksum_offset.to_le_bytes());
                    entry[64..68].copy_from_slice(&start.to_le_bytes());
                    entry[68..72].copy_from_slice(&length.to_le_bytes());
                }
            }
            bytes.extend_from_slice(&entry);
        }
        bytes
    }
}

fn validate_file(file: &str) -> Result<(), AcpiBuildError> {
    if file.is_empty() || file.len() >= 56 {
        return Err(AcpiBuildError::InvalidLoaderFile { name: file.into() });
    }
    Ok(())
}

fn write_file(destination: &mut [u8], file: &str) {
    destination[..file.len()].copy_from_slice(file.as_bytes());
}

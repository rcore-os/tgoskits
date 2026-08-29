use alloc::vec::Vec;
use core::num::NonZeroU16;

use sdmmc_protocol::sdio::io::{AddressMode, FunctionNumber, IoAddress, TransferMode};

use super::{AicError, SdioRequestKind, SdioResponse};
use crate::protocol::BLOCK_SIZE;

pub(super) fn expect_unit(response: SdioResponse) -> Result<(), AicError> {
    match response {
        SdioResponse::Unit => Ok(()),
        SdioResponse::Byte(_) | SdioResponse::Data(_) => Err(AicError::MalformedResponse),
    }
}

pub(super) fn expect_byte(response: SdioResponse) -> Result<u8, AicError> {
    match response {
        SdioResponse::Byte(value) => Ok(value),
        SdioResponse::Unit | SdioResponse::Data(_) => Err(AicError::MalformedResponse),
    }
}

pub(super) fn expect_write_readback(response: SdioResponse, expected: u8) -> Result<(), AicError> {
    let actual = expect_byte(response)?;
    if actual == expected {
        Ok(())
    } else {
        Err(AicError::SdioWriteReadbackMismatch { expected, actual })
    }
}

pub(super) fn expect_data(response: SdioResponse) -> Result<Vec<u8>, AicError> {
    match response {
        SdioResponse::Data(data) => Ok(data),
        SdioResponse::Unit | SdioResponse::Byte(_) => Err(AicError::MalformedResponse),
    }
}

pub(super) fn read_byte(function_number: u8, register: u32) -> SdioRequestKind {
    SdioRequestKind::ReadByte {
        function: function(function_number),
        address: address(register),
    }
}

pub(super) fn write_byte(function_number: u8, register: u32, value: u8) -> SdioRequestKind {
    SdioRequestKind::WriteByte {
        function: function(function_number),
        address: address(register),
        value,
        read_after_write: true,
    }
}

pub(super) fn read_fifo(function_number: u8, register: u32, length: usize) -> SdioRequestKind {
    SdioRequestKind::Read {
        function: function(function_number),
        address: address(register),
        address_mode: AddressMode::Fixed,
        transfer_mode: transfer_mode(length),
        length,
    }
}

pub(super) fn write_fifo(function_number: u8, register: u32, bytes: Vec<u8>) -> SdioRequestKind {
    SdioRequestKind::Write {
        function: function(function_number),
        address: address(register),
        address_mode: AddressMode::Fixed,
        transfer_mode: transfer_mode(bytes.len()),
        bytes,
    }
}

pub(super) fn function(number: u8) -> FunctionNumber {
    FunctionNumber::new(number).expect("AIC constants use valid SDIO functions")
}

fn address(value: u32) -> IoAddress {
    IoAddress::new(value).expect("AIC constants use valid SDIO addresses")
}

fn transfer_mode(length: usize) -> TransferMode {
    if length.is_multiple_of(BLOCK_SIZE) {
        TransferMode::Block {
            block_size: NonZeroU16::new(BLOCK_SIZE as u16)
                .expect("AIC protocol block size is non-zero"),
        }
    } else {
        TransferMode::Byte
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_request_uses_block_mode_for_aligned_payload() {
        let request = read_fifo(1, 0x08, BLOCK_SIZE * 2);
        assert!(matches!(
            request,
            SdioRequestKind::Read {
                transfer_mode: TransferMode::Block { block_size },
                length,
                ..
            } if block_size.get() as usize == BLOCK_SIZE && length == BLOCK_SIZE * 2
        ));
    }
}

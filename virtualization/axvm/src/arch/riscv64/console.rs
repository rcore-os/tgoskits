//! Task-side SBI console service over leased guest RAM.

use axvm_types::MappingFlags;
use sbi_spec::{binary::SbiRet, legacy};

use super::{
    AxvmRiscvHostOps, RiscvCompletion, RiscvSbiCall,
    policy::sbi_console::{
        EID_DBCN, FID_CONSOLE_READ, FID_CONSOLE_WRITE, FID_CONSOLE_WRITE_BYTE, console_read,
        console_write,
    },
};
use crate::services::RunServices;

pub(super) fn is_console_call(call: RiscvSbiCall) -> bool {
    matches!(
        call.extension,
        EID_DBCN | legacy::LEGACY_CONSOLE_PUTCHAR | legacy::LEGACY_CONSOLE_GETCHAR
    )
}

pub(super) fn handle(call: RiscvSbiCall, services: &RunServices) -> RiscvCompletion {
    if call.extension == legacy::LEGACY_CONSOLE_PUTCHAR {
        legacy_console(call.extension, call.arguments[0]);
        return RiscvCompletion::None;
    }
    if call.extension == legacy::LEGACY_CONSOLE_GETCHAR {
        return RiscvCompletion::Gpr {
            register: super::RiscvGprIndex::A0 as usize,
            value: legacy_console(call.extension, 0),
        };
    }
    let result = match call.function {
        FID_CONSOLE_WRITE_BYTE => sbi_rt::console_write_byte(call.arguments[0] as u8),
        FID_CONSOLE_READ | FID_CONSOLE_WRITE => copy_console(call, services),
        _ => SbiRet::not_supported(),
    };
    RiscvCompletion::SbiRet {
        error: result.error,
        value: result.value,
    }
}

fn copy_console(call: RiscvSbiCall, services: &RunServices) -> SbiRet {
    let [length, address, address_high, ..] = call.arguments;
    // DBCN address halves are XLEN-wide. This backend is RV64: a nonzero
    // upper half cannot name any address in the guest's GPA domain.
    if address_high != 0 || address.checked_add(length).is_none() {
        return SbiRet::invalid_param();
    }
    if length == 0 {
        return SbiRet::success(0);
    }
    // DBCN permits partial transfers. One bounded transfer avoids guest-driven
    // heap allocation while reporting the exact number consumed to the guest.
    let mut buffer = [0u8; 512];
    let count = length.min(buffer.len());
    let address = axvm_types::GuestPhysAddr::from_usize(address);
    let required = if call.function == FID_CONSOLE_WRITE {
        MappingFlags::READ
    } else {
        MappingFlags::WRITE
    };
    services
        .memory()
        .with_access_range(address, length, required, |memory| {
            if call.function == FID_CONSOLE_WRITE {
                if memory.read(address, &mut buffer[..count]).is_err() {
                    return SbiRet::invalid_param();
                }
                let result = console_write::<AxvmRiscvHostOps>(&buffer[..count]);
                if result.is_ok() && result.value > count {
                    return SbiRet::failed();
                }
                result
            } else {
                let result = console_read::<AxvmRiscvHostOps>(&mut buffer[..count]);
                if result.is_err() {
                    return result;
                }
                if result.value > count {
                    return SbiRet::failed();
                }
                if memory.write(address, &buffer[..result.value]).is_err() {
                    return SbiRet::invalid_param();
                }
                result
            }
        })
        .unwrap_or_else(|error| match error {
            axdevice_base::DeviceError::NotFound
            | axdevice_base::DeviceError::OutOfRange { .. }
            | axdevice_base::DeviceError::ReadOnly
            | axdevice_base::DeviceError::WriteOnly => SbiRet::invalid_param(),
            _ => SbiRet::failed(),
        })
}

fn legacy_console(extension: usize, argument: usize) -> usize {
    let result;
    // SAFETY: this is the SBI legacy register ABI. It receives only an owned
    // integer console argument and runs after guest binding and CPU pin retire.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") extension,
            inlateout("a0") argument => result,
        );
    }
    result
}

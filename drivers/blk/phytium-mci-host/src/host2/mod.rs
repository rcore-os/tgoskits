use super::*;

mod bus;
mod irq;
mod request;
mod transaction;

pub use bus::BusRequest;
pub use irq::PhytiumMciIrqHandle;
#[cfg(test)]
pub(crate) use request::supports_owned_dma_transaction;
pub use request::{DataRequest, TransactionRequest};

pub(crate) const PHYTIUM_REGISTER_RETRY_DELAY: Duration = Duration::from_micros(100);

fn register_pending<T>() -> sdmmc_host::RequestProgress<T> {
    sdmmc_host::RequestProgress::RegisterPending {
        retry_after: PHYTIUM_REGISTER_RETRY_DELAY,
    }
}

fn map_protocol_error(err: Error) -> sdmmc_host::Error {
    match err {
        Error::Timeout(_) => sdmmc_host::Error::Timeout,
        Error::Crc(_) => sdmmc_host::Error::Crc,
        Error::NoCard => sdmmc_host::Error::NoCard,
        Error::Busy => sdmmc_host::Error::Busy,
        Error::UnsupportedCommand => sdmmc_host::Error::Unsupported,
        Error::Misaligned => sdmmc_host::Error::Misaligned,
        Error::InvalidArgument => sdmmc_host::Error::InvalidArgument,
        Error::BusError(_) => sdmmc_host::Error::Bus,
        Error::ReadError(_) | Error::WriteError(_) | Error::BadResponse(_) => {
            sdmmc_host::Error::Bus
        }
        Error::CardError(_) | Error::CardLocked => sdmmc_host::Error::Controller,
        _ => sdmmc_host::Error::Controller,
    }
}

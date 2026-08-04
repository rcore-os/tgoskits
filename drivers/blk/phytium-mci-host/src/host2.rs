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

fn register_pending<T>() -> sdio_host2::RequestProgress<T> {
    sdio_host2::RequestProgress::RegisterPending {
        retry_after: PHYTIUM_REGISTER_RETRY_DELAY,
    }
}

fn map_protocol_error(err: Error) -> sdio_host2::Error {
    match err {
        Error::Timeout(_) => sdio_host2::Error::Timeout,
        Error::Crc(_) => sdio_host2::Error::Crc,
        Error::NoCard => sdio_host2::Error::NoCard,
        Error::Busy => sdio_host2::Error::Busy,
        Error::UnsupportedCommand => sdio_host2::Error::Unsupported,
        Error::Misaligned => sdio_host2::Error::Misaligned,
        Error::InvalidArgument => sdio_host2::Error::InvalidArgument,
        Error::BusError(_) => sdio_host2::Error::Bus,
        Error::ReadError(_) | Error::WriteError(_) | Error::BadResponse(_) => {
            sdio_host2::Error::Bus
        }
        Error::CardError(_) | Error::CardLocked => sdio_host2::Error::Controller,
        _ => sdio_host2::Error::Controller,
    }
}

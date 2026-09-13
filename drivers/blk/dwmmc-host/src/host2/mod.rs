use super::*;

mod bus;
mod irq;
mod request;
mod transaction;

pub use bus::BusRequest;
pub use irq::DwMmcIrq;
#[cfg(test)]
pub(crate) use irq::event_from_raw_status;
pub(crate) use irq::{
    DWMMC_INT_COMMAND_DONE, DWMMC_INT_DATA_TRANSFER_OVER, DWMMC_INT_ERROR_MASK,
    DWMMC_LATCH_IDMAC_COMPLETE, DWMMC_LATCH_IDMAC_ERROR,
};
pub use request::{DataRequest, TransactionRequest};

pub(crate) const DWMMC_REGISTER_RETRY_DELAY: Duration = Duration::from_micros(100);

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
